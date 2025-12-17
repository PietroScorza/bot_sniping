//! Helius Transaction Stream Client
//! 
//! This module provides a WebSocket-based client for streaming transactions
//! from Helius RPC with automatic reconnection.
//! Uses Solana's native PubSub instead of gRPC to avoid Windows build issues.

use anyhow::{Result, Context};
use futures::StreamExt;
use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use solana_client::rpc_config::{RpcTransactionLogsConfig, RpcTransactionLogsFilter, RpcTransactionConfig};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer, Signature};
use solana_sdk::transaction::VersionedTransaction;
use solana_transaction_status::UiTransactionEncoding;
use std::str::FromStr;
use std::time::Duration;
use std::sync::Arc;
use tokio::time::sleep;
use tracing::{info, warn, error, debug};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

/// Pump.fun program ID
pub const PUMPFUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// Wrapped SOL mint
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Detected trade action from logs
#[derive(Debug, Clone)]
pub enum DetectedAction {
    Buy { signature: String, slot: u64 },
    Sell { signature: String, slot: u64 },
    Unknown { signature: String, slot: u64 },
}

/// Represents a transaction log notification
#[derive(Debug, Clone)]
pub struct TransactionUpdate {
    /// Transaction signature
    pub signature: String,
    /// Slot number
    pub slot: u64,
    /// Whether the transaction was successful (no errors)
    pub is_success: bool,
}

/// Helius WebSocket client for streaming transactions
pub struct HeliusGrpcClient {
    ws_url: String,
    rpc_url: String,
    target_wallet: Pubkey,
    our_keypair: Arc<Keypair>,
    buy_amount_sol: f64,
    tip_amount: u64,
    reconnect_delay: Duration,
    max_reconnect_attempts: u32,
}

impl HeliusGrpcClient {
    /// Create a new Helius WebSocket client
    pub fn new(
        _endpoint: String,
        api_key: String,
        target_wallet: Pubkey,
        our_keypair: Arc<Keypair>,
        buy_amount_sol: f64,
        tip_amount: u64,
        reconnect_delay_ms: u64,
        max_reconnect_attempts: u32,
    ) -> Self {
        // Build WebSocket URL - Convert the RPC URL to WebSocket
        let ws_url = if api_key.starts_with("http") {
            api_key.replace("https://", "wss://").replace("http://", "ws://")
        } else {
            format!("wss://mainnet.helius-rpc.com/?api-key={}", api_key)
        };
        
        // Build RPC URL
        let rpc_url = if api_key.starts_with("http") {
            api_key.clone()
        } else {
            format!("https://mainnet.helius-rpc.com/?api-key={}", api_key)
        };
        
        Self {
            ws_url,
            rpc_url,
            target_wallet,
            our_keypair,
            buy_amount_sol,
            tip_amount,
            reconnect_delay: Duration::from_millis(reconnect_delay_ms),
            max_reconnect_attempts,
        }
    }
    
    /// Start streaming transactions and send them to the provided channel
    pub async fn stream_transactions(
        &self,
    ) -> Result<()> {
        let mut reconnect_attempts = 0;
        
        loop {
            match self.run_stream().await {
                Ok(_) => {
                    info!("Stream ended normally, reconnecting...");
                    reconnect_attempts = 0;
                }
                Err(e) => {
                    reconnect_attempts += 1;
                    error!(
                        "Stream error (attempt {}/{}): {:?}",
                        reconnect_attempts, self.max_reconnect_attempts, e
                    );
                    
                    if reconnect_attempts >= self.max_reconnect_attempts {
                        error!("Max reconnection attempts reached, giving up");
                        return Err(e);
                    }
                }
            }
            
            let backoff = self.reconnect_delay * reconnect_attempts;
            warn!("Reconnecting in {:?}...", backoff);
            sleep(backoff).await;
        }
    }
    
    /// Internal stream runner using WebSocket subscription
    async fn run_stream(&self) -> Result<()> {
        info!("Connecting to Helius WebSocket...");
        
        let pubsub_client = PubsubClient::new(&self.ws_url).await
            .context("Failed to connect to WebSocket")?;
        
        info!("✅ Connected to Helius WebSocket");
        info!("📡 Subscribing to logs for target wallet: {}", self.target_wallet);
        info!("💰 Buy amount: {} SOL | Tip: {} lamports", self.buy_amount_sol, self.tip_amount);
        info!("🔑 Our wallet: {}", self.our_keypair.pubkey());
        
        // Subscribe to logs mentioning the target wallet
        let (mut stream, _unsub) = pubsub_client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(vec![self.target_wallet.to_string()]),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::confirmed()),
                },
            )
            .await
            .context("Failed to subscribe to logs")?;
        
        info!("🎯 Listening for transactions from target wallet");
        
        // Process incoming log notifications
        while let Some(response) = stream.next().await {
            let signature = response.value.signature.clone();
            let slot = response.context.slot;
            let is_success = response.value.err.is_none();
            let logs = response.value.logs;
            
            if !is_success {
                debug!("❌ Failed transaction: {}", signature);
                continue;
            }
            
            // Analyze logs to detect Buy/Sell
            let action = self.detect_action_from_logs(&logs, &signature, slot);
            
            match action {
                DetectedAction::Buy { signature, slot } => {
                    info!("🎯 TARGET BUY DETECTED! Signature: {} (slot: {})", signature, slot);
                    
                    // Try to extract token mint from logs first
                    let token_mint = if let Some(mint) = self.extract_token_from_logs(&logs) {
                        Some(mint)
                    } else {
                        // Fallback: fetch full transaction to get token mint
                        info!("🔍 Fetching full transaction to find token mint...");
                        match self.extract_token_from_transaction(&signature).await {
                            Ok(mint) => mint,
                            Err(e) => {
                                warn!("Failed to fetch transaction: {:?}", e);
                                None
                            }
                        }
                    };
                    
                    if let Some(mint) = token_mint {
                        info!("🪙 Token mint: {}", mint);
                        
                        // Execute copy buy
                        match self.execute_copy_buy(&mint, &signature).await {
                            Ok(our_sig) => {
                                info!("✅ COPY BUY EXECUTED! Our signature: {}", our_sig);
                            }
                            Err(e) => {
                                error!("❌ Copy buy failed: {:?}", e);
                            }
                        }
                    } else {
                        warn!("⚠️ Could not extract token mint from buy transaction");
                    }
                }
                DetectedAction::Sell { signature, slot } => {
                    info!("🚨 TARGET SELL DETECTED! Signature: {} (slot: {})", signature, slot);
                    
                    // For sells, we would need to track our positions
                    let token_mint = if let Some(mint) = self.extract_token_from_logs(&logs) {
                        Some(mint)
                    } else {
                        match self.extract_token_from_transaction(&signature).await {
                            Ok(mint) => mint,
                            Err(_) => None,
                        }
                    };
                    
                    if let Some(mint) = token_mint {
                        info!("🪙 Token being sold: {}", mint);
                        warn!("⚠️ Auto-sell not yet implemented - manual action may be needed!");
                    }
                }
                DetectedAction::Unknown { signature, slot } => {
                    debug!("📋 Other transaction from target: {} (slot: {})", signature, slot);
                }
            }
        }
        
        Ok(())
    }
    
    /// Detect if transaction is a Buy or Sell from logs
    fn detect_action_from_logs(&self, logs: &[String], signature: &str, slot: u64) -> DetectedAction {
        let logs_str = logs.join(" ");
        
        // Check for Pump.fun Buy
        if logs_str.contains("Instruction: Buy") && logs_str.contains(PUMPFUN_PROGRAM) {
            return DetectedAction::Buy {
                signature: signature.to_string(),
                slot,
            };
        }
        
        // Check for Pump.fun Sell
        if logs_str.contains("Instruction: Sell") && logs_str.contains(PUMPFUN_PROGRAM) {
            return DetectedAction::Sell {
                signature: signature.to_string(),
                slot,
            };
        }
        
        // Check for Raydium/Jupiter swaps (simplified)
        if logs_str.contains("swap") || logs_str.contains("Swap") {
            // Try to determine direction from context
            if logs_str.contains("SOL") {
                // Could be buy or sell, need more context
            }
        }
        
        DetectedAction::Unknown {
            signature: signature.to_string(),
            slot,
        }
    }
    
    /// Extract token mint from transaction logs
    fn extract_token_from_logs(&self, logs: &[String]) -> Option<String> {
        // Look for token mint in logs - Pump.fun logs contain mint info
        // Format: "Program log: <mint_pubkey>"
        for log in logs {
            // Skip non-relevant logs
            if log.contains("Program log:") {
                let parts: Vec<&str> = log.split_whitespace().collect();
                for part in parts {
                    // Check if it's a valid pubkey (32 bytes base58)
                    if part.len() >= 32 && part.len() <= 44 {
                        if let Ok(pubkey) = Pubkey::from_str(part) {
                            // Exclude known programs and SOL
                            let pubkey_str = pubkey.to_string();
                            if pubkey_str != PUMPFUN_PROGRAM 
                                && pubkey_str != WSOL_MINT
                                && pubkey_str != self.target_wallet.to_string()
                                && !pubkey_str.starts_with("1111") // system program variants
                            {
                                return Some(pubkey_str);
                            }
                        }
                    }
                }
            }
        }
        None
    }
    
    /// Extract token mint by fetching full transaction details
    async fn extract_token_from_transaction(&self, signature: &str) -> Result<Option<String>> {
        let rpc_client = AsyncRpcClient::new(self.rpc_url.clone());
        
        let sig = Signature::from_str(signature)
            .context("Invalid signature")?;
        
        // Fetch full transaction with metadata
        let tx_config = RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::JsonParsed),
            commitment: Some(CommitmentConfig::confirmed()),
            max_supported_transaction_version: Some(0),
        };
        
        let tx = rpc_client.get_transaction_with_config(&sig, tx_config).await?;
        
        // Parse the transaction to find token mints from metadata
        if let Some(meta) = tx.transaction.meta {
            // Use skip_serialization for OptionSerializer
            use solana_transaction_status::option_serializer::OptionSerializer;
            
            // Look for token balance changes
            if let OptionSerializer::Some(post_token_balances) = meta.post_token_balances {
                for balance in post_token_balances.iter() {
                    let mint_str = balance.mint.clone();
                    if mint_str != WSOL_MINT {
                        return Ok(Some(mint_str));
                    }
                }
            }
        }
        
        Ok(None)
    }

    /// Execute a copy buy transaction using Jupiter
    async fn execute_copy_buy(&self, token_mint: &str, _target_signature: &str) -> Result<String> {
        let rpc_client = AsyncRpcClient::new(self.rpc_url.clone());
        
        let token_mint_pubkey = Pubkey::from_str(token_mint)
            .context("Invalid token mint")?;
        
        info!("🔄 Building copy buy transaction for token: {}", token_mint);
        
        let buy_amount_lamports = (self.buy_amount_sol * 1_000_000_000.0) as u64;
        
        // Use Jupiter API to get swap quote and transaction
        let client = reqwest::Client::new();
        
        // Step 1: Get quote from Jupiter
        let quote_url = format!(
            "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint={}&amount={}&slippageBps=2000",
            WSOL_MINT,
            token_mint,
            buy_amount_lamports
        );
        
        info!("📊 Getting Jupiter quote...");
        let quote_response = client.get(&quote_url)
            .send()
            .await
            .context("Failed to get Jupiter quote")?;
        
        if !quote_response.status().is_success() {
            let error_text = quote_response.text().await.unwrap_or_default();
            error!("Jupiter quote error: {}", error_text);
            return Err(anyhow::anyhow!("Jupiter quote failed: {}", error_text));
        }
        
        let quote_data: serde_json::Value = quote_response.json().await
            .context("Failed to parse Jupiter quote")?;
        
        info!("✅ Got quote: {:?}", quote_data.get("outAmount"));
        
        // Step 2: Get swap transaction from Jupiter
        let swap_url = "https://quote-api.jup.ag/v6/swap";
        let swap_request = serde_json::json!({
            "quoteResponse": quote_data,
            "userPublicKey": self.our_keypair.pubkey().to_string(),
            "wrapAndUnwrapSol": true,
            "dynamicComputeUnitLimit": true,
            "prioritizationFeeLamports": self.tip_amount
        });
        
        info!("🔨 Building swap transaction...");
        let swap_response = client.post(swap_url)
            .json(&swap_request)
            .send()
            .await
            .context("Failed to get Jupiter swap")?;
        
        if !swap_response.status().is_success() {
            let error_text = swap_response.text().await.unwrap_or_default();
            error!("Jupiter swap error: {}", error_text);
            return Err(anyhow::anyhow!("Jupiter swap failed: {}", error_text));
        }
        
        let swap_data: serde_json::Value = swap_response.json().await
            .context("Failed to parse Jupiter swap")?;
        
        // Extract the serialized transaction
        let swap_transaction = swap_data.get("swapTransaction")
            .and_then(|v| v.as_str())
            .context("No swap transaction in response")?;
        
        // Decode and sign the transaction
        let tx_bytes = BASE64.decode(swap_transaction)
            .context("Failed to decode transaction")?;
        
        let mut versioned_tx: VersionedTransaction = bincode::deserialize(&tx_bytes)
            .context("Failed to deserialize transaction")?;
        
        // Sign the transaction
        info!("✍️ Signing transaction...");
        let message_bytes = versioned_tx.message.serialize();
        let signature = self.our_keypair.sign_message(&message_bytes);
        versioned_tx.signatures[0] = signature;
        
        // Send the transaction
        info!("📤 Sending transaction...");
        let serialized_tx = bincode::serialize(&versioned_tx)
            .context("Failed to serialize signed transaction")?;
        
        let sig = rpc_client.send_transaction(&versioned_tx).await
            .context("Failed to send transaction")?;
        
        info!("🚀 Transaction sent: {}", sig);
        
        Ok(sig.to_string())
    }
}

/// Simplified client builder
pub struct HeliusClientBuilder {
    endpoint: Option<String>,
    api_key: Option<String>,
    target_wallet: Option<Pubkey>,
    our_keypair: Option<Arc<Keypair>>,
    buy_amount_sol: f64,
    tip_amount: u64,
    reconnect_delay_ms: u64,
    max_reconnect_attempts: u32,
}

impl Default for HeliusClientBuilder {
    fn default() -> Self {
        Self {
            endpoint: None,
            api_key: None,
            target_wallet: None,
            our_keypair: None,
            buy_amount_sol: 0.1,
            tip_amount: 10_000,
            reconnect_delay_ms: 1000,
            max_reconnect_attempts: 10,
        }
    }
}

impl HeliusClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }
    
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }
    
    pub fn target_wallet(mut self, wallet: Pubkey) -> Self {
        self.target_wallet = Some(wallet);
        self
    }
    
    pub fn keypair(mut self, keypair: Arc<Keypair>) -> Self {
        self.our_keypair = Some(keypair);
        self
    }
    
    pub fn buy_amount_sol(mut self, amount: f64) -> Self {
        self.buy_amount_sol = amount;
        self
    }
    
    pub fn tip_amount(mut self, tip: u64) -> Self {
        self.tip_amount = tip;
        self
    }
    
    pub fn reconnect_delay_ms(mut self, delay: u64) -> Self {
        self.reconnect_delay_ms = delay;
        self
    }
    
    pub fn max_reconnect_attempts(mut self, attempts: u32) -> Self {
        self.max_reconnect_attempts = attempts;
        self
    }
    
    pub fn build(self) -> Result<HeliusGrpcClient> {
        Ok(HeliusGrpcClient::new(
            self.endpoint.context("Endpoint is required")?,
            self.api_key.context("API key is required")?,
            self.target_wallet.context("Target wallet is required")?,
            self.our_keypair.context("Keypair is required")?,
            self.buy_amount_sol,
            self.tip_amount,
            self.reconnect_delay_ms,
            self.max_reconnect_attempts,
        ))
    }
}
