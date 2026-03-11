//! Smol BTC Wallet - Rust CLI
//! A BIP86 Taproot wallet using BDK + rust-bitcoin

mod api;
mod wallet;

use bitcoin::{Address, Amount, Network};
use bip39::Mnemonic;
use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use api::{MempoolApi, identify_rare_sat};
use wallet::{Wallet, create_send_psbt, parse_psbt, parse_psbt_from_bytes, serialize_psbt};

fn get_wallet_dir() -> PathBuf {
    let home = dirs::home_dir().expect("No home directory");
    home.join(".config").join("btc-wallet")
}

fn get_wallet_file() -> PathBuf {
    get_wallet_dir().join("wallet.json")
}

#[derive(Parser)]
#[command(name = "btc-wallet")]
#[command(about = "Smol BTC Wallet - BIP86 Taproot Wallet CLI", long_about = None)]
#[command(version = "0.0.2")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    
    #[arg(long, default_value = "false")]
    testnet: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a new BIP39 mnemonic and wallet
    New,
    /// Import wallet from BIP39 mnemonic
    Import { mnemonic: String },
    /// Show wallet address
    Address,
    /// Show wallet balance
    Balance,
    /// List unspent transaction outputs (UTXOs)
    Utxos {
        /// Show values in sats (default: BTC)
        #[arg(long, default_value = "false")]
        sats: bool,
    },
    /// Send BTC to an address
    Send {
        destination: String,
        amount: u64,
        /// Interactive coin selection (pick which UTXOs to spend)
        #[arg(long = "coin-select", help = "Interactive coin selection (UTXO picker)", default_value = "false")]
        coin_select: bool,
    },
    /// Send all balance to an address (minus fee)
    SendAll {
        destination: String,
        /// Interactive coin selection (pick which UTXOs to spend)
        #[arg(long = "coin-select", help = "Interactive coin selection (UTXO picker)", default_value = "false")]
        coin_select: bool,
    },
    /// Sign a PSBT file with BIP86 key
    SignPsbt {
        psbt_file: String,
        /// Output file path for signed PSBT
        #[arg(long)]
        output: Option<String>,
    },
    /// Decode and display PSBT details
    DecodePsbt {
        psbt_file: String,
    },
    /// Broadcast a signed PSBT to the network
    Broadcast {
        psbt_file: String,
    },
    /// Clear/delete wallet from storage
    Clear,
    /// Show wallet information
    Info,
    /// Analyze UTXOs for ordinals/inscriptions/rare sats
    Ordinals,
    /// Sweep UTXOs to destination (protects rare/inscribed by default)
    Sweep {
        destination: String,
        /// Include UTXOs with inscriptions (DANGER: may lose valuable inscriptions!)
        #[arg(long = "include-inscribed", help = "Include UTXOs with inscriptions (dangerous!)", default_value = "false")]
        include_inscribed: bool,
        /// Include rare sats (DANGER: may lose valuable sats!)
        #[arg(long = "include-rare", help = "Include rare sats (dangerous!)", default_value = "false")]
        include_rare: bool,
        /// Minimum UTXO value to include in sats
        #[arg(long = "min-value", help = "Minimum UTXO value in sats", default_value = "0")]
        min_value: u64,
    },
    /// Show derivation path information
    Derive {
        /// Display the BIP86 derivation path
        #[arg(long, default_value = "false")]
        show_path: bool,
    },
}

use api::Utxo;
use std::io::{self, Write};

fn interactive_coin_selection(
    rt: &tokio::runtime::Runtime,
    api: &MempoolApi,
    wallet: &Wallet,
    dest_address: &Address,
    amount: u64,
    is_send_all: bool,
) -> Result<(), String> {
    let mut utxos = rt.block_on(api.fetch_utxos(wallet.get_address().to_string().as_str()))?;
    
    if utxos.is_empty() {
        return Err("No UTXOs available".to_string());
    }
    
    // Get ordinal info
    let block_height = rt.block_on(api.get_block_height()).unwrap_or(0);
    for utxo in &mut utxos {
        utxo.has_inscription = rt.block_on(api.check_inscription(&utxo.txid, utxo.vout)).unwrap_or(false);
        utxo.rare_info = identify_rare_sat(utxo.value, block_height);
    }
    
    // All selected by default
    let mut selected: Vec<bool> = vec![true; utxos.len()];
    let mut current = 0usize;
    
    loop {
        // Clear screen (simplified)
        print!("\x1B[2J\x1B[H");
        
        let total_selected: u64 = utxos.iter().enumerate()
            .filter(|(i, _)| selected[*i])
            .map(|(_, u)| u.value)
            .sum();
        
        let fee = 1000u64;
        let send_amount = if is_send_all { total_selected.saturating_sub(fee) } else { amount };
        
        let mode_str = if is_send_all { "SEND ALL".to_string() } else { format!("SEND {} sats", amount) };
        println!("\n🪙 COIN CONTROL - {}", mode_str);
        println!("   Destination: {}", dest_address);
        println!("   Selected: {} UTXOs / {} total", selected.iter().filter(|&&s| s).count(), utxos.len());
        println!("   Total: {} sats", total_selected);
        if !is_send_all {
            println!("   Need: {} sats", amount);
            println!("   Status: {} {}", 
                if total_selected >= amount { "✅ OK" } else { "❌ INSUFFICIENT" },
                if total_selected >= amount { "" } else { "(need more)" }
            );
        } else {
            println!("   Will send: {} sats (after {} fee)", send_amount, fee);
        }
        
        println!("\n{:3} {:8} {:12} {:25} {}", 
            "#", "Status", "Value", "Features", "TXID:VOUT");
        println!("{}", "-".repeat(75));
        
        for (i, u) in utxos.iter().enumerate() {
            let marker = if i == current { ">" } else { " " };
            let sel = if selected[i] { "[✓]" } else { "[ ]" };
            let sel_marker = if i == current { "→" } else { " " };
            
            let mut features = String::new();
            if u.has_inscription { features.push_str("📜 "); }
            if let Some(ref r) = u.rare_info { features.push_str(&format!("⭐ ")); }
            if features.is_empty() { features = "normal".to_string(); }
            
            let txid_short = format!("{}:{}", &u.txid[..8], u.vout);
            println!("{}{:2} {} {:12} {:25} {}", marker, i + 1, sel, u.value, features, txid_short);
        }
        
        println!("\n[1-{}] Toggle  [a]ll  [n]one  [r]are/inscribed  [Enter] Send  [q]uit", utxos.len());
        
        print!("\n> ");
        io::stdout().flush().ok();
        
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        let input = input.trim();
        
        match input {
            "q" | "Q" => {
                println!("\n❌ Cancelled.");
                return Ok(());
            }
            "" => {
                // Enter - proceed
                let selected_utxos: Vec<_> = utxos.iter().enumerate()
                    .filter(|(i, _)| selected[*i])
                    .map(|(_, u)| u.clone())
                    .collect();
                
                if selected_utxos.is_empty() {
                    println!("\n⚠️  No UTXOs selected!");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
                
                let total_sel: u64 = selected_utxos.iter().map(|u| u.value).sum();
                if !is_send_all && total_sel < amount {
                    println!("\n⚠️  Selected ({} sats) < amount ({} sats)", total_sel, amount);
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
                
                let send_amt = if is_send_all { total_sel.saturating_sub(fee) } else { amount };
                
                if send_amt == 0 || (is_send_all && total_sel <= fee) {
                    println!("\n⚠️  Not enough for fee!");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
                
                println!("\n✅ Sending {} sats to {}...", send_amt, dest_address);
                
                let inputs: Vec<_> = selected_utxos.iter().map(|u| {
                    let txid = bitcoin::Txid::from_str(&u.txid).expect("Invalid txid");
                    let script = wallet.get_address().script_pubkey().as_bytes().to_vec();
                    (txid, u.vout, Amount::from_sat(u.value), script)
                }).collect();
                
                let psbt = create_send_psbt(
                    &inputs,
                    dest_address,
                    Amount::from_sat(send_amt),
                    wallet.get_address(),
                    bitcoin::Network::Bitcoin,
                ).map_err(|e| format!("Failed to create PSBT: {}", e))?;
                
                let mut psbt = psbt;
                let signed = wallet.sign_psbt(&mut psbt).map_err(|e| e.to_string())?;
                println!("   ✍️  Signed {} input(s)", signed);
                
                wallet.finalize_psbt(&mut psbt).map_err(|e| e.to_string())?;
                
                let tx_hex = bitcoin::consensus::encode::serialize(&psbt.extract_tx().unwrap())
                    .iter().map(|b| format!("{:02x}", b)).collect::<String>();
                
                let txid = rt.block_on(api.broadcast_tx(&tx_hex)).map_err(|e| e.to_string())?;
                println!("\n✅ Broadcast successful!");
                println!("   TXID: {}", txid);
                return Ok(());
            }
            "a" | "A" => {
                for s in selected.iter_mut() { *s = true; }
            }
            "n" | "N" => {
                for s in selected.iter_mut() { *s = false; }
            }
            "r" | "R" => {
                for (i, u) in utxos.iter().enumerate() {
                    selected[i] = u.has_inscription || u.rare_info.is_some();
                }
            }
            _ => {
                if let Ok(idx) = input.parse::<usize>() {
                    if idx >= 1 && idx <= utxos.len() {
                        selected[idx - 1] = !selected[idx - 1];
                        current = idx - 1;
                    }
                }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let network = if cli.testnet { Network::Testnet } else { Network::Bitcoin };
    
    let wallet_dir = get_wallet_dir();
    fs::create_dir_all(&wallet_dir)?;
    
    let wallet = load_wallet(network)?;
    
    match cli.command {
        Commands::New => {
            println!("\n🪙 Generating new wallet...\n");
            let mnemonic = Wallet::generate_mnemonic();
            let wallet = Wallet::new(mnemonic.clone(), network)?;
            
            println!("✅ New wallet created!");
            println!("\n🔐 Mnemonic (save this safely!):");
            println!("   {}\n", mnemonic.to_string());
            println!("📍 Address: {}\n", wallet.get_address());
            
            let wallet_file = get_wallet_file();
            let wallet_json = serde_json::json!({
                "mnemonic": mnemonic.to_string(),
                "network": if cli.testnet { "testnet" } else { "mainnet" },
                "address": wallet.get_address().to_string(),
            });
            fs::write(&wallet_file, serde_json::to_string_pretty(&wallet_json)?)?;
            println!("💾 Wallet saved to {}\n", wallet_file.display());
        }
        
        Commands::Import { mnemonic } => {
            println!("\n🪙 Importing wallet...\n");
            
            if !Wallet::validate_mnemonic(&mnemonic) {
                eprintln!("❌ Invalid mnemonic\n");
                std::process::exit(1);
            }
            
            let mnemonic = Mnemonic::parse(&mnemonic)
                .map_err(|e| format!("Invalid mnemonic: {}", e))?;
            let wallet = Wallet::new(mnemonic.clone(), network)?;
            
            println!("✅ Wallet imported!");
            println!("📍 Address: {}\n", wallet.get_address());
            
            let wallet_file = get_wallet_file();
            let wallet_json = serde_json::json!({
                "mnemonic": mnemonic.to_string(),
                "network": if cli.testnet { "testnet" } else { "mainnet" },
                "address": wallet.get_address().to_string(),
            });
            fs::write(&wallet_file, serde_json::to_string_pretty(&wallet_json)?)?;
            println!("💾 Wallet saved to {}\n", wallet_file.display());
        }
        
        Commands::Address => {
            if let Some(w) = &wallet {
                println!("{}\n", w.get_address());
            } else {
                eprintln!("❌ No wallet loaded\n");
                std::process::exit(1);
            }
        }
        
        Commands::Balance => {
            let w = match &wallet {
                Some(w) => w,
                None => { eprintln!("❌ No wallet loaded\n"); std::process::exit(1); }
            };
            
            println!("\n📡 Fetching balance...\n");
            let api = MempoolApi::new(cli.testnet);
            
            let rt = tokio::runtime::Runtime::new()?;
            let utxos = rt.block_on(api.fetch_utxos(w.get_address().to_string().as_str()))?;
            
            let total: u64 = utxos.iter().map(|u| u.value).sum();
            println!("💰 {} sats ({} UTXOs)\n", total, utxos.len());
        }
        
        Commands::Utxos { sats: _ } => {
            let w = match &wallet {
                Some(w) => w,
                None => { eprintln!("❌ No wallet loaded\n"); std::process::exit(1); }
            };
            
            println!("\n📡 Fetching UTXOs for {}...\n", w.get_address());
            let api = MempoolApi::new(cli.testnet);
            
            let rt = tokio::runtime::Runtime::new()?;
            let utxos = rt.block_on(api.fetch_utxos(w.get_address().to_string().as_str()))?;
            
            if utxos.is_empty() {
                println!("No UTXOs.\n");
                return Ok(());
            }
            
            println!("Found {} UTXOs:\n", utxos.len());
            println!(" #   TXID:VOUT                    VALUE");
            println!("{}", "-".repeat(50));
            
            for (i, u) in utxos.iter().enumerate() {
                println!("{}   {}:{}  {} sats", 
                    format!("{:>2}", i + 1),
                    u.txid,
                    u.vout,
                    u.value
                );
            }
            println!();
        }
        
        Commands::Send { destination, amount, coin_select } => {
            let w = match &wallet {
                Some(w) => w,
                None => { eprintln!("❌ No wallet loaded\n"); std::process::exit(1); }
            };
            
            let dest_address = Address::from_str(&destination)
                .map_err(|e| format!("Invalid address: {}", e))?;
            
            let dest_address = dest_address.require_network(network)
                .map_err(|e| format!("Invalid address: {}", e))?;
            
            println!("\n📡 Fetching UTXOs...\n");
            let api = MempoolApi::new(cli.testnet);
            
            let rt = tokio::runtime::Runtime::new()?;
            let mut utxos = rt.block_on(api.fetch_utxos(w.get_address().to_string().as_str()))?;
            
            if utxos.is_empty() {
                eprintln!("❌ No UTXOs available\n");
                std::process::exit(1);
            }
            
            // Get ordinal info for each UTXO
            let block_height = rt.block_on(api.get_block_height()).unwrap_or(0);
            for utxo in &mut utxos {
                utxo.has_inscription = rt.block_on(api.check_inscription(&utxo.txid, utxo.vout)).unwrap_or(false);
                utxo.rare_info = identify_rare_sat(utxo.value, block_height);
            }
            
            if coin_select {
                // Interactive coin selection
                interactive_coin_selection(&rt, &api, &w, &dest_address, amount, false)?;
            } else {
                // Default: use all UTXOs
                let inputs: Vec<_> = utxos.iter().map(|u| {
                    let txid = bitcoin::Txid::from_str(&u.txid).expect("Invalid txid");
                    let script = w.get_address().script_pubkey().as_bytes().to_vec();
                    (txid, u.vout, Amount::from_sat(u.value), script)
                }).collect();
                
                let amount_sat = Amount::from_sat(amount);
                let psbt = create_send_psbt(
                    &inputs,
                    &dest_address,
                    amount_sat,
                    w.get_address(),
                    network,
                ).map_err(|e| format!("Failed to create PSBT: {}", e))?;
                
                // Sign and broadcast
                let mut psbt = psbt;
                let signed = w.sign_psbt(&mut psbt)?;
                println!("   ✍️  Signed {} input(s)", signed);
                
                w.finalize_psbt(&mut psbt)?;
                
                let tx_hex = bitcoin::consensus::encode::serialize(&psbt.extract_tx().unwrap()).iter().map(|b| format!("{:02x}", b)).collect::<String>();
                
                let txid = rt.block_on(api.broadcast_tx(&tx_hex))?;
                println!("\n✅ Broadcast successful!");
                println!("   TXID: {}", txid);
                println!("   {}\n", if cli.testnet { 
                    format!("https://mempool.space/testnet/tx/{}", txid)
                } else {
                    format!("https://mempool.space/tx/{}", txid)
                });
            }
        }
        
        Commands::SendAll { destination, coin_select } => {
            let w = match &wallet {
                Some(w) => w,
                None => { eprintln!("❌ No wallet loaded\n"); std::process::exit(1); }
            };
            
            let dest_address = Address::from_str(&destination)
                .map_err(|e| format!("Invalid address: {}", e))?;
            
            let dest_address = dest_address.require_network(network)
                .map_err(|e| format!("Invalid address: {}", e))?;
            
            println!("\n📡 Fetching UTXOs...\n");
            let api = MempoolApi::new(cli.testnet);
            
            let rt = tokio::runtime::Runtime::new()?;
            let mut utxos = rt.block_on(api.fetch_utxos(w.get_address().to_string().as_str()))?;
            
            if utxos.is_empty() {
                eprintln!("❌ No UTXOs available\n");
                std::process::exit(1);
            }
            
            let block_height = rt.block_on(api.get_block_height()).unwrap_or(0);
            for utxo in &mut utxos {
                utxo.has_inscription = rt.block_on(api.check_inscription(&utxo.txid, utxo.vout)).unwrap_or(false);
                utxo.rare_info = identify_rare_sat(utxo.value, block_height);
            }
            
            if coin_select {
                interactive_coin_selection(&rt, &api, &w, &dest_address, 0, true)?;
            } else {
                let total: u64 = utxos.iter().map(|u| u.value).sum();
                let fee = 1000u64;
                let amount = total.saturating_sub(fee);
                
                if amount == 0 {
                    eprintln!("❌ Not enough balance for fee\n");
                    std::process::exit(1);
                }
                
                let inputs: Vec<_> = utxos.iter().map(|u| {
                    let txid = bitcoin::Txid::from_str(&u.txid).expect("Invalid txid");
                    let script = w.get_address().script_pubkey().as_bytes().to_vec();
                    (txid, u.vout, Amount::from_sat(u.value), script)
                }).collect();
                
                let psbt = create_send_psbt(
                    &inputs,
                    &dest_address,
                    Amount::from_sat(amount),
                    w.get_address(),
                    network,
                ).map_err(|e| format!("Failed to create PSBT: {}", e))?;
                
                let mut psbt = psbt;
                let signed = w.sign_psbt(&mut psbt)?;
                println!("   ✍️  Signed {} input(s)", signed);
                
                w.finalize_psbt(&mut psbt)?;
                
                let tx_hex = bitcoin::consensus::encode::serialize(&psbt.extract_tx().unwrap()).iter().map(|b| format!("{:02x}", b)).collect::<String>();
                
                let txid = rt.block_on(api.broadcast_tx(&tx_hex))?;
                println!("\n✅ Broadcast successful!");
                println!("   TXID: {}", txid);
                println!("   {}\n", if cli.testnet { 
                    format!("https://mempool.space/testnet/tx/{}", txid)
                } else {
                    format!("https://mempool.space/tx/{}", txid)
                });
            }
        }
        
        Commands::SignPsbt { psbt_file, output } => {
            let w = match &wallet {
                Some(w) => w,
                None => { eprintln!("❌ No wallet loaded\n"); std::process::exit(1); }
            };
            
            // Read as bytes to detect format
            let psbt_bytes = fs::read(&psbt_file)
                .map_err(|e| format!("Failed to read PSBT file: {}", e))?;
            
            let mut psbt = if psbt_bytes.len() >= 4 
                && psbt_bytes[0] == 0x70  // 'p'
                && psbt_bytes[1] == 0x73  // 's'
                && psbt_bytes[2] == 0x62  // 'b'
                && psbt_bytes[3] == 0x74  // 't'
            {
                // Binary PSBT
                parse_psbt_from_bytes(&psbt_bytes)?
            } else {
                // Base64 PSBT
                let psbt_str = String::from_utf8_lossy(&psbt_bytes);
                parse_psbt(psbt_str.trim())?
            };
            
            println!("\n🔐 Signing PSBT with BIP86 key...\n");
            
            let signed = w.sign_psbt(&mut psbt)?;
            println!("   Signed {} input(s)", signed);
            
            w.finalize_psbt(&mut psbt)?;
            println!("   Finalized PSBT");
            
            let signed_base64 = serialize_psbt(&psbt)?;
            
            if let Some(output_path) = output {
                fs::write(&output_path, &signed_base64)?;
                println!("\n💾 Signed PSBT saved to: {}\n", output_path);
            } else {
                println!("\n📝 Signed PSBT:\n");
                for chunk in signed_base64.as_bytes().chunks(64) {
                    println!("{}", String::from_utf8_lossy(chunk));
                }
                println!();
            }
            
            println!("💡 Broadcast with: btc-wallet broadcast <psbt-file>\n");
        }
        
        Commands::DecodePsbt { psbt_file } => {
            let psbt_data: String = match fs::read_to_string(&psbt_file) {
                Ok(s) => s,
                Err(_) => {
                    let bytes = fs::read(&psbt_file)
                        .map_err(|e| format!("Failed to read PSBT file: {}", e))?;
                    String::from_utf8_lossy(&bytes).to_string()
                }
            };
            
            let psbt = parse_psbt(psbt_data.trim())?;
            
            println!("\n📋 PSBT Decoded:\n");
            println!("   Inputs: {}", psbt.inputs.len());
            println!("   Outputs: {}", psbt.outputs.len());
            
            println!("\n📥 Inputs:");
            for (i, input) in psbt.inputs.iter().enumerate() {
                if input.witness_utxo.is_some() {
                    println!("   {}: Witness UTXO", i + 1);
                    if input.tap_key_sig.is_some() {
                        println!("      ✅ Taproot signature present");
                    }
                } else {
                    println!("   {}: Non-witness", i + 1);
                }
            }
            
            println!("\n📤 Outputs:");
            for (i, output) in psbt.unsigned_tx.output.iter().enumerate() {
                let value = output.value;
                println!("   {}: {} sats", i + 1, value.to_sat());
            }
            println!();
        }
        
        Commands::Broadcast { psbt_file } => {
            // Read as bytes to detect format
            let psbt_bytes = fs::read(&psbt_file)
                .map_err(|e| format!("Failed to read PSBT file: {}", e))?;
            
            let mut psbt = if psbt_bytes.len() >= 4 
                && psbt_bytes[0] == 0x70
                && psbt_bytes[1] == 0x73
                && psbt_bytes[2] == 0x62
                && psbt_bytes[3] == 0x74
            {
                parse_psbt_from_bytes(&psbt_bytes)?
            } else {
                let psbt_str = String::from_utf8_lossy(&psbt_bytes);
                parse_psbt(psbt_str.trim())?
            };
            
            let has_sigs = psbt.inputs.iter().any(|i| i.tap_key_sig.is_some() || i.final_script_witness.is_some());
            if !has_sigs {
                eprintln!("❌ PSBT not signed\n");
                std::process::exit(1);
            }
            
            let tx = psbt.extract_tx().map_err(|e| format!("Failed to extract tx: {}", e))?;
            let tx_hex = hex::encode(bitcoin::consensus::encode::serialize(&tx));
            
            println!("\n📡 Broadcasting to {}...\n", if cli.testnet { "testnet" } else { "mainnet" });
            
            let api = MempoolApi::new(cli.testnet);
            let rt = tokio::runtime::Runtime::new()?;
            
            match rt.block_on(api.broadcast_tx(&tx_hex)) {
                Ok(txid) => {
                    println!("✅ Broadcast successful!");
                    println!("   TXID: {}", txid);
                    if cli.testnet {
                        println!("   https://mempool.space/testnet/tx/{}\n", txid);
                    } else {
                        println!("   https://mempool.space/tx/{}\n", txid);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Broadcast failed: {}\n", e);
                    std::process::exit(1);
                }
            }
        }
        
        Commands::Clear => {
            let wallet_file = get_wallet_file();
            if wallet_file.exists() {
                fs::remove_file(&wallet_file)?;
                println!("🗑️ Wallet deleted\n");
            } else {
                println!("No wallet to clear\n");
            }
        }
        
        Commands::Info => {
            if let Some(w) = &wallet {
                println!("\n📍 Address: {}", w.get_address());
                println!("   Type: BIP86 Taproot (P2TR)");
                println!("   XPub: {}\n", w.get_descriptor());
            } else {
                println!("\n❌ No wallet loaded\n");
            }
        }
        
        Commands::Ordinals => {
            println!("\n🔍 Analyzing UTXOs for ordinals/inscriptions...\n");
            
            let w = match &wallet {
                Some(w) => w,
                None => { eprintln!("❌ No wallet loaded\n"); std::process::exit(1); }
            };
            
            let api = MempoolApi::new(cli.testnet);
            let rt = tokio::runtime::Runtime::new()?;
            let utxos = rt.block_on(api.fetch_utxos(w.get_address().to_string().as_str()))?;
            
            if utxos.is_empty() {
                println!("No UTXOs found.");
                return Ok(());
            }
            
            let block_height = rt.block_on(api.get_block_height()).unwrap_or(0);
            
            let mut inscribed = 0;
            let mut rare = 0;
            
            for utxo in &utxos {
                let has_inscription = rt.block_on(api.check_inscription(&utxo.txid, utxo.vout)).unwrap_or(false);
                let rare_info = identify_rare_sat(utxo.value, block_height);
                
                if has_inscription {
                    inscribed += 1;
                    println!("📜 INSCRIBED");
                    println!("   {}:{} - {} sats\n", utxo.txid, utxo.vout, utxo.value);
                } else if rare_info.is_some() {
                    rare += 1;
                    let r = rare_info.unwrap();
                    println!("⭐ RARE: {}", r.0);
                    println!("   {}:{} - {} sats", utxo.txid, utxo.vout, utxo.value);
                    println!("   {}\n", r.1);
                }
            }
            
            let normal = utxos.len() - inscribed - rare;
            println!("\n📊 Summary:");
            println!("   Inscribed: {}", inscribed);
            println!("   Rare: {}", rare);
            println!("   Normal: {}", normal);
            println!("   Total: {}\n", utxos.len());
        }
        
        Commands::Sweep { destination, include_inscribed, include_rare, min_value } => {
            println!("\n🧹 Sweeping UTXOs to {}...", destination);
            println!("   Exclude inscribed: {}", !include_inscribed);
            println!("   Exclude rare sats: {}", !include_rare);
            println!("   Min value: {} sats\n", min_value);
            
            let w = match &wallet {
                Some(w) => w,
                None => { eprintln!("❌ No wallet loaded\n"); std::process::exit(1); }
            };
            
            let dest_address = Address::from_str(&destination)
                .map_err(|e| format!("Invalid address: {}", e))?;
            let dest_address = dest_address.require_network(network)
                .map_err(|e| format!("Invalid address for network: {}", e))?;
            
            let api = MempoolApi::new(cli.testnet);
            let rt = tokio::runtime::Runtime::new()?;
            let all_utxos = rt.block_on(api.fetch_utxos(w.get_address().to_string().as_str()))?;
            
            if all_utxos.is_empty() {
                println!("No UTXOs found.");
                return Ok(());
            }
            
            let block_height = rt.block_on(api.get_block_height()).unwrap_or(0);
            
            let mut eligible_utxos = Vec::new();
            let mut total_value = 0u64;
            
            for utxo in &all_utxos {
                if utxo.value < min_value {
                    println!("   ⏭️  Skipping {}:{} - below min value ({} sats)", 
                        utxo.txid, utxo.vout, utxo.value);
                    continue;
                }
                
                let has_inscription = rt.block_on(api.check_inscription(&utxo.txid, utxo.vout)).unwrap_or(false);
                if !include_inscribed && has_inscription {
                    println!("   ⛔ Excluding {}:{} - has inscription", utxo.txid, utxo.vout);
                    continue;
                }
                
                let rare_info = identify_rare_sat(utxo.value, block_height);
                if !include_rare && rare_info.is_some() {
                    let r = rare_info.unwrap();
                    println!("   ⛔ Excluding {}:{} - {} ({})", utxo.txid, utxo.vout, r.0, r.1);
                    continue;
                }
                
                println!("   ✅ Including {}:{} - {} sats", utxo.txid, utxo.vout, utxo.value);
                eligible_utxos.push(utxo.clone());
                total_value += utxo.value;
            }
            
            if eligible_utxos.is_empty() {
                println!("\n❌ No eligible UTXOs to sweep.");
                return Ok(());
            }
            
            let fee = 1000u64;
            let sweep_amount = total_value.saturating_sub(fee);
            
            println!("\n📊 Sweeping {} UTXOs totaling {} sats", eligible_utxos.len(), total_value);
            println!("   Sweep amount: {} sats (after {} sats fee)\n", sweep_amount, fee);
            
            let inputs: Vec<_> = eligible_utxos.iter().map(|u| {
                let txid = bitcoin::Txid::from_str(&u.txid).expect("Invalid txid");
                let script = w.get_address().script_pubkey().as_bytes().to_vec();
                (txid, u.vout, Amount::from_sat(u.value), script)
            }).collect();
            
            let psbt = create_send_psbt(
                &inputs,
                &dest_address,
                Amount::from_sat(sweep_amount),
                w.get_address(),
                network,
            ).map_err(|e| format!("Failed to create PSBT: {}", e))?;
            
            let mut psbt = psbt;
            let signed = w.sign_psbt(&mut psbt)?;
            println!("   ✍️  Signed {} input(s)", signed);
            
            w.finalize_psbt(&mut psbt)?;
            println!("   Finalized PSBT");
            
            let _serialized = serialize_psbt(&psbt)?;
            let tx = psbt.extract_tx().unwrap();
            let tx_hex = bitcoin::consensus::encode::serialize(&tx).iter().map(|b| format!("{:02x}", b)).collect::<String>();
            
            let txid = rt.block_on(api.broadcast_tx(&tx_hex))?;
            println!("\n✅ Broadcast successful!");
            println!("   TXID: {}", txid);
            println!("   {}\n", if cli.testnet { 
                format!("https://mempool.space/testnet/tx/{}", txid)
            } else {
                format!("https://mempool.space/tx/{}", txid)
            });
        }
        
        Commands::Derive { show_path } => {
            if show_path {
                let path = if network == Network::Testnet { "m/86'/1'/0'/0/0" } else { "m/86'/0'/0'/0/0" };
                println!("\n🔑 BIP86 Derivation Path:");
                println!("   {}", path);
                println!("   (testnet: m/86'/1'/0'/0/0, mainnet: m/86'/0'/0'/0/0)\n");
            }
            if let Some(w) = &wallet {
                println!("\n📍 Address: {}", w.get_address());
                println!("   Derivation: BIP86");
                println!("   Network: {}\n", network);
            } else {
                println!("\n❌ No wallet loaded\n");
            }
        }
    }
    
    Ok(())
}

fn load_wallet(network: Network) -> Result<Option<Wallet>, Box<dyn std::error::Error>> {
    let wallet_file = get_wallet_file();
    
    if !wallet_file.exists() {
        return Ok(None);
    }
    
    let content = fs::read_to_string(&wallet_file)?;
    let wallet_json: serde_json::Value = serde_json::from_str(&content)?;
    
    let mnemonic_str = wallet_json["mnemonic"]
        .as_str()
        .ok_or("Missing mnemonic in wallet file")?;
    
    let wallet_network = if wallet_json["network"]
        .as_str()
        .map(|s| s == "testnet")
        .unwrap_or(false)
    {
        Network::Testnet
    } else {
        Network::Bitcoin
    };
    
    let mnemonic = Mnemonic::parse(mnemonic_str)?;
    let wallet = Wallet::new(mnemonic, network)?;
    
    Ok(Some(wallet))
}

