//! Wallet operations: derivation, signing, PSBT handling

use bitcoin::{
    address::Address, 
    network::Network, 
    psbt::{Psbt, Output as PsbtOutput}, 
    secp256k1::{Keypair, Secp256k1, SecretKey},
    Amount, Transaction, TxOut, Txid, XOnlyPublicKey, absolute::LockTime,
    taproot::Signature as TaprootSignature
};
use bitcoin::bip32::{DerivationPath, Xpriv, Xpub};
use bitcoin_hashes::Hash;
use bip39::{Mnemonic, Language};
use std::str::FromStr;

/// BIP86 derivation path: m/86'/0'/0'/0/0 (mainnet) or m/86'/1'/0'/0/0 (testnet)
const BIP86_MAINNET_PATH: &str = "m/86'/0'/0'/0/0";
const BIP86_TESTNET_PATH: &str = "m/86'/1'/0'/0/0";

pub struct Wallet {
    pub mnemonic: Mnemonic,
    pub network: Network,
    pub address: Address,
    pub keypair: Keypair,
    pub xpub: Xpub,
}

impl Wallet {
    /// Create a new wallet from mnemonic
    pub fn new(mnemonic: Mnemonic, network: Network) -> Result<Self, String> {
        // Get seed from mnemonic
        let seed = mnemonic.to_seed("");
        
        // Derive the correct path based on network
        let path = match network {
            Network::Bitcoin => BIP86_MAINNET_PATH,
            Network::Testnet | Network::Signet | Network::Regtest => BIP86_TESTNET_PATH,
            _ => BIP86_MAINNET_PATH,
        };
        
        let derivation_path = DerivationPath::from_str(path)
            .map_err(|e| format!("Invalid derivation path: {}", e))?;
        
        // Create extended private key from seed
        let xpriv = Xpriv::new_master(network, &seed)
            .map_err(|e| format!("Failed to create master key: {}", e))?;
        
        // Derive to the BIP86 path
        let secp = Secp256k1::new();
        
        let derived = xpriv
            .derive_priv(&secp, &derivation_path)
            .map_err(|e| format!("Derivation failed: {}", e))?;
        
        // DEBUG: Print the derived key
        let keypair = derived.to_keypair(&secp);
        println!("DEBUG pubkey: {}", keypair.public_key());
        
        // Create x-only public key (for Taproot)
        // from_keypair returns (XOnlyPublicKey, Parity), we only need the first
        let (x_only_pubkey, _parity) = XOnlyPublicKey::from_keypair(&keypair);
        
        // Create P2TR address (Taproot - BIP86)
        let address = Address::p2tr(
            &secp,
            x_only_pubkey,
            None, // No tap tree hash
            network,
        );
        
        // Get the extended public key
        let xpub = Xpub::from_priv(&secp, &derived);
        
        Ok(Self {
            mnemonic,
            network,
            address,
            keypair,
            xpub,
        })
    }
    
    /// Generate a new random mnemonic
    pub fn generate_mnemonic() -> Mnemonic {
        Mnemonic::generate(24)
            .expect("Failed to generate mnemonic")
    }
    
    /// Validate a mnemonic
    pub fn validate_mnemonic(mnemonic: &str) -> bool {
        Mnemonic::parse(mnemonic).is_ok()
    }
    
    /// Get the wallet address
    pub fn get_address(&self) -> &Address {
        &self.address
    }
    
    /// Get the private key as hex
    pub fn get_private_key_hex(&self) -> String {
        hex::encode(self.keypair.secret_bytes())
    }
    
    /// Sign a PSBT for BIP86 Taproot inputs
    pub fn sign_psbt(&self, psbt: &mut Psbt) -> Result<usize, String> {
        use bitcoin::sighash::{SighashCache, TapSighashType, TapSighash, Prevouts};
        use bitcoin::key::TapTweak;
        
        
        let secp = Secp256k1::new();
        
        let mut signed_count = 0;
        
        // Get our xonly public key (internal key)
        let (our_internal_key, _) = XOnlyPublicKey::from_keypair(&self.keypair);
        
        // Apply BIP341 key tweak to get the spending key
        let tweaked_keypair = self.keypair.tap_tweak(&secp, None);
        
        // Build prevouts - need all input outputs
        let prevout_values: Vec<bitcoin::TxOut> = psbt.inputs.iter()
            .filter_map(|input| input.witness_utxo.clone())
            .collect();
        
        if prevout_values.is_empty() {
            return Err("No witness_utxo found in PSBT inputs".to_string());
        }
        
        for (i, input) in psbt.inputs.iter_mut().enumerate() {
            if let Some(witness_utxo) = &input.witness_utxo {
                let script_pubkey = &witness_utxo.script_pubkey;
                let script_bytes = script_pubkey.as_bytes();
                
                // Check if it's P2TR (OP_1 followed by 32 bytes)
                if script_bytes.len() == 34 && script_bytes[0] == 0x51 && script_bytes[1] == 0x20 {
                    // Set tap_internal_key to our INTERNAL key (critical for BIP86)
                    if input.tap_internal_key.is_none() {
                        input.tap_internal_key = Some(our_internal_key);
                    }
                    
                    // Create sighash
                    let mut sighash_cache = SighashCache::new(&psbt.unsigned_tx);
                    let prevouts = Prevouts::All(&prevout_values);
                    
                    // Compute BIP341 sighash for key path spending
                    let sighash: TapSighash = sighash_cache
                        .taproot_key_spend_signature_hash(i, &prevouts, TapSighashType::Default)
                        .map_err(|e| format!("Sighash failed: {}", e))?;
                    
                    // Sign with TWEAKED key
                    let msg = bitcoin::secp256k1::Message::from_digest(sighash.to_byte_array());
                    let tweaked_secret = tweaked_keypair.to_inner();
                    let sig = secp.sign_schnorr(&msg, &tweaked_secret);
                    
                    // Create Taproot signature
                    let tap_sig = TaprootSignature {
                        signature: sig,
                        sighash_type: TapSighashType::Default,
                    };
                    
                    input.tap_key_sig = Some(tap_sig);
                    signed_count += 1;
                }
            }
        }
        
        Ok(signed_count)
    }
    
    /// Finalize a signed PSBT - for Taproot this means setting final_script_witness
    pub fn finalize_psbt(&self, psbt: &mut Psbt) -> Result<(), String> {
        use bitcoin::Witness;
        
        // For Taproot BIP86 key-path spending
        for input in psbt.inputs.iter_mut() {
            if let Some(tap_sig) = &input.tap_key_sig {
                // Get the signature bytes
                let sig_bytes = tap_sig.to_vec();
                
                // Set the final witness - for BIP86 key-path it's just [signature]
                let mut witness = Witness::new();
                witness.push(&sig_bytes);
                input.final_script_witness = Some(witness);
                
                // CRITICAL: Clear signing data - BIP174 requirement before broadcast
                // The signature is now in final_script_witness
                input.tap_key_sig = None;
                input.tap_internal_key = None;
            }
        }
        
        Ok(())
    }
    
    /// Extract the transaction from a finalized PSBT
    pub fn extract_tx(&self, psbt: &Psbt) -> Result<Transaction, String> {
        psbt.clone()
            .extract_tx()
            .map_err(|e| format!("Failed to extract transaction: {}", e))
    }
    
    /// Get the xpub for the wallet (for descriptors)
    pub fn get_xpub(&self) -> String {
        format!("{}", self.xpub)
    }
    
    /// Get the descriptor for this wallet
    pub fn get_descriptor(&self) -> String {
        let network_suffix = match self.network {
            Network::Bitcoin => "0",
            Network::Testnet | Network::Signet | Network::Regtest => "1",
            _ => "0",
        };
        
        format!(
            "tr([{}{}]{{pub}}/0/0/*)",
            self.xpub.fingerprint().to_string(),
            network_suffix
        )
    }
}

/// Create a PSBT for sending BTC
pub fn create_send_psbt(
    utxos: &[(Txid, u32, Amount, Vec<u8>)],  // (txid, vout, value, script)
    destination: &Address,
    amount: Amount,
    change_address: &Address,
    _network: Network,
) -> Result<Psbt, String> {
    use bitcoin::blockdata::transaction::{Transaction, TxIn, TxOut as BTxOut};
    use bitcoin::transaction::Version;
    
    // Create a dummy transaction to initialize the PSBT
    let mut tx = Transaction {
        version: Version(2),
        lock_time: LockTime::ZERO,
        input: vec![],
        output: vec![],
    };
    
    // Add input placeholders
    for (txid, vout, _, _) in utxos.iter() {
        tx.input.push(TxIn {
            previous_output: bitcoin::OutPoint::new(*txid, *vout),
            sequence: bitcoin::Sequence::MAX,
            witness: bitcoin::Witness::new(),
            script_sig: bitcoin::ScriptBuf::new(),
        });
    }
    
    // Add output placeholders
    tx.output.push(BTxOut {
        value: amount,
        script_pubkey: destination.script_pubkey(),
    });
    
    let mut psbt = Psbt::from_unsigned_tx(tx)
        .map_err(|e| format!("Failed to create PSBT: {}", e))?;
    
    // Calculate total input
    let total_input: u64 = utxos.iter().map(|(_, _, v, _)| v.to_sat()).sum();
    
    // Calculate fee (rough estimate: 150 vbytes per input + 2 outputs)
    let fee_rate = bitcoin::FeeRate::from_sat_per_vb_unchecked(1);
    let estimated_vsize = (utxos.len() * 150) as u64 + 200;
    let fee = fee_rate.fee_vb(estimated_vsize)
        .ok_or("Failed to calculate fee")?;
    
    // Check we have enough
    if amount.to_sat() + fee.to_sat() > total_input {
        return Err("Insufficient funds".to_string());
    }
    
    // Set the witness UTXO for each input
    for (i, (_txid, _vout, value, script)) in utxos.iter().enumerate() {
        let tx_out = BTxOut {
            value: *value,
            script_pubkey: script.clone().into(),
        };
        
        psbt.inputs[i].witness_utxo = Some(tx_out);
    }
    
    // Update the first output - for PSBT output we set witness_script and tap_internal_key
    // The actual output info goes in unsigned_tx.output
    psbt.unsigned_tx.output[0].script_pubkey = destination.script_pubkey();
    
    // Add change output if needed
    let change_amount = Amount::from_sat(total_input - amount.to_sat() - fee.to_sat());
    if change_amount.to_sat() > 546 { // Dust threshold
        let change_out = BTxOut {
            value: change_amount,
            script_pubkey: change_address.script_pubkey(),
        };
        
        // Add to outputs
        let psbt_output = PsbtOutput::default();
        psbt.outputs.push(psbt_output);
        
        // Add to transaction
        psbt.unsigned_tx.output.push(change_out);
    }
    
    Ok(psbt)
}

/// Parse a PSBT from base64
pub fn parse_psbt(psbt_base64: &str) -> Result<Psbt, String> {
    use base64::Engine;
    
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(psbt_base64)
        .map_err(|e| format!("Failed to decode base64: {}", e))?;
    
    Psbt::deserialize(&bytes)
        .map_err(|e| format!("Failed to parse PSBT: {}", e))
}

/// Parse a PSBT from raw bytes (binary format)
pub fn parse_psbt_from_bytes(bytes: &[u8]) -> Result<Psbt, String> {
    Psbt::deserialize(bytes)
        .map_err(|e| format!("Failed to parse binary PSBT: {}", e))
}

/// Serialize a PSBT to base64
pub fn serialize_psbt(psbt: &Psbt) -> Result<String, String> {
    use base64::Engine;
    
    let bytes = psbt.serialize();
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}
