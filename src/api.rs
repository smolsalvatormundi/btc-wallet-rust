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
