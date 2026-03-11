//! Mempool.space API client

use bitcoin::{Address, Amount, Transaction, Txid};
use serde::{Deserialize, Serialize};

const MAINNET_API: &str = "https://mempool.space/api";
const TESTNET_API: &str = "https://mempool.space/testnet/api";

pub struct MempoolApi {
    base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utxo {
    pub txid: String,
    pub vout: u32,
    pub value: u64,
    pub status: Option<UtxoStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoStatus {
    pub confirmed: bool,
    pub block_height: Option<u64>,
    pub block_hash: Option<String>,
    pub block_time: Option<u64>,
}

impl MempoolApi {
    pub fn new(testnet: bool) -> Self {
        Self {
            base_url: if testnet { TESTNET_API.to_string() } else { MAINNET_API.to_string() },
        }
    }

    /// Fetch UTXOs for an address
    pub async fn fetch_utxos(&self, address: &str) -> Result<Vec<Utxo>, String> {
        let url = format!("{}/address/{}/utxo", self.base_url, address);
        
        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        
        if !response.status().is_success() {
            return Err(format!("API error: {}", response.status()));
        }
        
        let utxos: Vec<Utxo> = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse UTXOs: {}", e))?;
        
        Ok(utxos)
    }

    /// Get current fee estimates
    pub async fn get_fee_estimates(&self) -> Result<FeeEstimates, String> {
        let url = format!("{}/fees/recommended", self.base_url);
        
        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        
        if !response.status().is_success() {
            return Err(format!("API error: {}", response.status()));
        }
        
        let fees: FeeEstimates = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse fees: {}", e))?;
        
        Ok(fees)
    }

    /// Broadcast a transaction
    pub async fn broadcast_tx(&self, tx_hex: &str) -> Result<String, String> {
        let url = format!("{}/tx", self.base_url);
        
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .body(tx_hex.to_string())
            .header("Content-Type", "text/plain")
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        
        if !response.status().is_success() {
            let error = response.text().await.unwrap_or_default();
            return Err(format!("Broadcast failed: {}", error));
        }
        
        let txid = response.text().await.map_err(|e| format!("Failed to read response: {}", e))?;
        
        Ok(txid)
    }

    /// Get transaction details
    pub async fn get_tx(&self, txid: &str) -> Result<Transaction, String> {
        let url = format!("{}/tx/{}", self.base_url, txid);
        
        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        
        if !response.status().is_success() {
            return Err(format!("API error: {}", response.status()));
        }
        
        let hex: String = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse tx: {}", e))?;
        
        let bytes = hex::decode(&hex).map_err(|e| format!("Failed to decode hex: {}", e))?;
        
        let tx: Transaction = bitcoin::consensus::encode::deserialize(&bytes)
            .map_err(|e| format!("Failed to parse transaction: {}", e))?;
        
        Ok(tx)
    }

    /// Get address info
    pub async fn get_address_info(&self, address: &str) -> Result<AddressInfo, String> {
        let url = format!("{}/address/{}", self.base_url, address);
        
        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        
        if !response.status().is_success() {
            return Err(format!("API error: {}", response.status()));
        }
        
        let info: AddressInfo = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse address info: {}", e))?;
        
        Ok(info)
    }
    
    /// Check if a UTXO has an inscription
    pub async fn check_inscription(&self, txid: &str, vout: u32) -> Result<bool, String> {
        let url = format!("{}/tx/{}/outspend/{}", self.base_url, txid, vout);
        
        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        
        if !response.status().is_success() {
            return Ok(false);
        }
        
        let outspend: Option<OutSpend> = response.json().await.ok();
        
        Ok(outspend.map(|o| o.inscription.is_some()).unwrap_or(false))
    }
    
    /// Get current block height
    pub async fn get_block_height(&self) -> Result<u64, String> {
        let url = format!("{}/blocks/tip/height", self.base_url);
        
        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        
        if !response.status().is_success() {
            return Err(format!("API error: {}", response.status()));
        }
        
        let height: u64 = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse block height: {}", e))?;
        
        Ok(height)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeEstimates {
    pub fastest_fee: f64,
    pub half_hour_fee: f64,
    pub hour_fee: f64,
    pub economy_fee: f64,
    pub minimum_fee: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressInfo {
    pub address: String,
    pub chain_stats: ChainStats,
    pub mempool_stats: MempoolStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainStats {
    pub funded_txo_count: u64,
    pub spent_txo_count: u64,
    pub total_sats: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MempoolStats {
    pub funded_txo_count: u64,
    pub spent_txo_count: u64,
    pub total_sats: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutSpend {
    pub spent: bool,
    pub txid: Option<String>,
    pub vin: Option<u32>,
    pub status: Option<OutSpendStatus>,
    pub inscription: Option<Inscription>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutSpendStatus {
    pub confirmed: bool,
    pub block_height: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inscription {
    pub id: String,
    pub number: i64,
}

/// Identify if a satoshi is rare based on ordinal theory
pub fn identify_rare_sat(sat_position: u64, _block_height: u64) -> Option<(String, String)> {
    // Genesis sat
    if sat_position == 0 {
        return Some(("genesis".to_string(), "Genesis Sat - First satoshi ever created".to_string()));
    }
    
    let sats_per_block = 100;
    let sat_in_block = sat_position % sats_per_block;
    
    // First sat of block
    if sat_in_block == 0 {
        let block_num = sat_position / sats_per_block;
        return Some(("block".to_string(), format!("Block Founder - First sat of block {}", block_num)));
    }
    
    // Last sat of block
    if sat_in_block == sats_per_block - 1 {
        let block_num = sat_position / sats_per_block;
        return Some(("block-end".to_string(), format!("Block End - Last sat of block {}", block_num)));
    }
    
    // Collector sats (ends with repeating digits)
    let sat_str = sat_position.to_string();
    if sat_str.len() >= 3 {
        let last_three = &sat_str[sat_str.len().saturating_sub(3)..];
        if last_three.chars().all(|c| c == last_three.chars().next().unwrap()) {
            return Some(("collector".to_string(), format!("Collector Sat - Ends with {}", last_three)));
        }
    }
    
    // Round numbers
    if sat_str.len() >= 5 {
        if sat_str.starts_with('1') && sat_str.chars().skip(1).all(|c| c == '0') {
            return Some(("round".to_string(), format!("Round Sat - {} sats", sat_position)));
        }
    }
    
    None
}
