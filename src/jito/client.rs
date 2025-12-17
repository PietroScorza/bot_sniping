//! Jito Block Engine client for bundle submission

use anyhow::{Result, Context};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use solana_sdk::signature::Signature;
use std::time::Duration;
use tracing::{info, warn, error, debug};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use super::bundle::JitoBundle;

/// Jito Block Engine client
pub struct JitoClient {
    /// HTTP client for bundle submission
    http_client: Client,
    /// Block engine URL
    block_engine_url: String,
    /// Bundle submission timeout
    timeout: Duration,
}

impl JitoClient {
    /// Create a new Jito client
    pub fn new(block_engine_url: String) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");
        
        Self {
            http_client,
            block_engine_url,
            timeout: Duration::from_secs(30),
        }
    }
    
    /// Set custom timeout
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    
    /// Submit a bundle to Jito Block Engine via HTTP
    pub async fn submit_bundle(&self, bundle: &JitoBundle) -> Result<BundleSubmissionResult> {
        let serialized_txs: Vec<String> = bundle.transactions.iter()
            .map(|tx| {
                let bytes = bincode::serialize(tx)?;
                Ok(BASE64.encode(bytes))
            })
            .collect::<Result<Vec<_>>>()?;
        
        let request = SendBundleRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "sendBundle".to_string(),
            params: vec![serialized_txs],
        };
        
        let url = format!("{}/api/v1/bundles", self.block_engine_url);
        debug!("Submitting bundle to: {}", url);
        
        let response = self.http_client
            .post(&url)
            .json(&request)
            .timeout(self.timeout)
            .send()
            .await
            .context("Failed to send bundle request")?;
        
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            error!("Bundle submission failed: {} - {}", status, body);
            anyhow::bail!("Bundle submission failed: {} - {}", status, body);
        }
        
        let result: SendBundleResponse = response.json().await
            .context("Failed to parse bundle response")?;
        
        if let Some(error) = result.error {
            error!("Bundle error: {:?}", error);
            return Ok(BundleSubmissionResult {
                bundle_id: None,
                success: false,
                error: Some(error.message),
            });
        }
        
        let bundle_id = result.result;
        info!("✅ Bundle submitted successfully: {:?}", bundle_id);
        
        Ok(BundleSubmissionResult {
            bundle_id,
            success: true,
            error: None,
        })
    }
    
    /// Check bundle status
    pub async fn get_bundle_status(&self, bundle_id: &str) -> Result<BundleStatus> {
        let request = GetBundleStatusRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "getBundleStatuses".to_string(),
            params: vec![vec![bundle_id.to_string()]],
        };
        
        let url = format!("{}/api/v1/bundles", self.block_engine_url);
        
        let response = self.http_client
            .post(&url)
            .json(&request)
            .timeout(self.timeout)
            .send()
            .await
            .context("Failed to get bundle status")?;
        
        let result: GetBundleStatusResponse = response.json().await
            .context("Failed to parse bundle status response")?;
        
        if let Some(statuses) = result.result {
            if let Some(status) = statuses.value.first() {
                return Ok(status.clone());
            }
        }
        
        Ok(BundleStatus {
            bundle_id: bundle_id.to_string(),
            transactions: vec![],
            slot: 0,
            confirmation_status: "unknown".to_string(),
            err: None,
        })
    }
    
    /// Get tip accounts from Jito
    pub async fn get_tip_accounts(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/v1/bundles/tip_accounts", self.block_engine_url);
        
        let response = self.http_client
            .get(&url)
            .timeout(self.timeout)
            .send()
            .await
            .context("Failed to get tip accounts")?;
        
        let result: TipAccountsResponse = response.json().await
            .context("Failed to parse tip accounts response")?;
        
        Ok(result.accounts)
    }
    
    /// Send a transaction via Jito (single transaction, not bundle)
    pub async fn send_transaction(&self, tx_base64: &str) -> Result<String> {
        let request = SendTransactionRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "sendTransaction".to_string(),
            params: vec![tx_base64.to_string()],
        };
        
        let url = format!("{}/api/v1/transactions", self.block_engine_url);
        
        let response = self.http_client
            .post(&url)
            .json(&request)
            .timeout(self.timeout)
            .send()
            .await
            .context("Failed to send transaction")?;
        
        let result: SendTransactionResponse = response.json().await
            .context("Failed to parse transaction response")?;
        
        result.result.context("No signature returned")
    }
}

/// Bundle submission result
#[derive(Debug)]
pub struct BundleSubmissionResult {
    pub bundle_id: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}

/// Bundle status
#[derive(Debug, Clone, Deserialize)]
pub struct BundleStatus {
    pub bundle_id: String,
    pub transactions: Vec<String>,
    pub slot: u64,
    pub confirmation_status: String,
    pub err: Option<serde_json::Value>,
}

// JSON-RPC request/response types

#[derive(Serialize)]
struct SendBundleRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Vec<Vec<String>>,
}

#[derive(Deserialize)]
struct SendBundleResponse {
    result: Option<String>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Serialize)]
struct GetBundleStatusRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Vec<Vec<String>>,
}

#[derive(Deserialize)]
struct GetBundleStatusResponse {
    result: Option<BundleStatusResult>,
}

#[derive(Deserialize)]
struct BundleStatusResult {
    value: Vec<BundleStatus>,
}

#[derive(Deserialize)]
struct TipAccountsResponse {
    accounts: Vec<String>,
}

#[derive(Serialize)]
struct SendTransactionRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Vec<String>,
}

#[derive(Deserialize)]
struct SendTransactionResponse {
    result: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_client_creation() {
        let client = JitoClient::new("https://test.jito.wtf".to_string());
        assert_eq!(client.block_engine_url, "https://test.jito.wtf");
    }
}
