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
use wallet::{Wallet, create_send_psbt, parse_psbt, serialize_psbt};

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
struct Cli {
    #[command(subcommand)]
    command: Commands,
    
    #[arg(long, default_value = "false")]
    testnet: bool,
}

#[derive(Subcommand)]
enum Commands {
    New,
    Import { mnemonic: String },
    Address,
    Balance,
    Utxos {
        #[arg(long, default_value = "false")]
        sats: bool,
    },
    Send {
        destination: String,
        amount: u64,
    },
    SignPsbt {
        psbt_file: String,
        #[arg(long)]
        output: Option<String>,
    },
    DecodePsbt {
        psbt_file: String,
    },
    Broadcast {
        psbt_file: String,
    },
    Clear,
    Info,
    Ordinals,
    Sweep {
        destination: String,
        #[arg(long, default_value = "false")]
        include_inscribed: bool,
        #[arg(long, default_value = "false")]
        include_rare: bool,
        #[arg(long, default_value = "0")]
        min_value: u64,
    },
    Derive {
        #[arg(long, default_value = "false")]
        show_path: bool,
    },
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
        
        Commands::Send { destination, amount } => {
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
            let utxos = rt.block_on(api.fetch_utxos(w.get_address().to_string().as_str()))?;
            
            if utxos.is_empty() {
                eprintln!("❌ No UTXOs available\n");
                std::process::exit(1);
            }
            
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
            
            println!("✅ PSBT created!");
            println!("   Destination: {}", dest_address);
            println!("   Amount: {} sats", amount);
            println!("   Inputs: {}", inputs.len());
            
            let psbt_base64 = serialize_psbt(&psbt)?;
            println!("\n📝 Unsigned PSBT (base64):\n");
            for chunk in psbt_base64.as_bytes().chunks(64) {
                println!("{}", String::from_utf8_lossy(chunk));
            }
            println!("\n💡 Import this PSBT into Sparrow/Hardware Wallet to sign.\n");
            println!("   Then use: btc-wallet sign-psbt <signed-psbt-file>\n");
        }
        
        Commands::SignPsbt { psbt_file, output } => {
            let w = match &wallet {
                Some(w) => w,
                None => { eprintln!("❌ No wallet loaded\n"); std::process::exit(1); }
            };
            
            let psbt_data: String = match fs::read_to_string(&psbt_file) {
                Ok(s) => s,
                Err(_) => {
                    let bytes = fs::read(&psbt_file)
                        .map_err(|e| format!("Failed to read PSBT file: {}", e))?;
                    String::from_utf8_lossy(&bytes).to_string()
                }
            };
            
            let psbt_base64 = psbt_data.trim();
            let mut psbt = parse_psbt(psbt_base64)?;
            
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
            let psbt_data: String = match fs::read_to_string(&psbt_file) {
                Ok(s) => s,
                Err(_) => {
                    let bytes = fs::read(&psbt_file)
                        .map_err(|e| format!("Failed to read PSBT file: {}", e))?;
                    String::from_utf8_lossy(&bytes).to_string()
                }
            };
            
            let mut psbt = parse_psbt(psbt_data.trim())?;
            
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
            let tx_hex = psbt.extract_tx().unwrap().to_hex();
            
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

