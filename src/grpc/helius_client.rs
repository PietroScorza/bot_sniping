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
use std::collections::HashMap;
use tokio::time::sleep;
use tokio::sync::RwLock;
use tracing::{info, warn, error, debug};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use crate::config::TakeProfitTier;

/// Pump.fun program ID
pub const PUMPFUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// Wrapped SOL mint
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Pump.fun API endpoints (faster than Jupiter for pump tokens)
pub const PUMPFUN_TRADE_API: &str = "https://pumpportal.fun/api/trade-local";
pub const PUMPFUN_QUOTE_API: &str = "https://pumpportal.fun/api/quote";

/// Jito Block Engine endpoints (for MEV priority)
pub const JITO_MAINNET_BLOCK_ENGINE: &str = "https://mainnet.block-engine.jito.wtf";
pub const JITO_AMSTERDAM_BLOCK_ENGINE: &str = "https://amsterdam.mainnet.block-engine.jito.wtf";
pub const JITO_FRANKFURT_BLOCK_ENGINE: &str = "https://frankfurt.mainnet.block-engine.jito.wtf";
pub const JITO_NY_BLOCK_ENGINE: &str = "https://ny.mainnet.block-engine.jito.wtf";
pub const JITO_TOKYO_BLOCK_ENGINE: &str = "https://tokyo.mainnet.block-engine.jito.wtf";

/// Position info for take profit tracking
#[derive(Debug, Clone)]
pub struct PositionInfo {
    pub token_mint: String,
    pub entry_sol: f64,        // SOL spent to buy
    pub token_amount: u64,     // Tokens received
    pub entry_time: std::time::Instant,
    /// Total percent of the original position already sold via TP (0-100)
    pub sold_percent: u8,
}

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
    /// Active positions for take profit monitoring (token_mint -> PositionInfo)
    positions: Arc<RwLock<HashMap<String, PositionInfo>>>,
    /// Take profit enabled
    take_profit_enabled: bool,
    /// Take profit tiers (multiplier + cumulative sell_percent)
    take_profit_tiers: Vec<TakeProfitTier>,
    /// Use Jito Block Engine for MEV priority
    use_jito: bool,
    /// Jito Block Engine URL
    jito_url: String,
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
        take_profit_enabled: bool,
        take_profit_tiers: Vec<TakeProfitTier>,
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
            positions: Arc::new(RwLock::new(HashMap::new())),
            take_profit_enabled,
            take_profit_tiers,
            use_jito: false, // Disabled - was "Jito light", not real bundles
            jito_url: JITO_MAINNET_BLOCK_ENGINE.to_string(),
        }
    }
    
    /// Send transaction via RPC - fast and simple, no waiting for confirmation
    /// We fire and forget - checking confirmation adds latency
    async fn send_transaction_fast(
        &self,
        rpc_client: &AsyncRpcClient,
        versioned_tx: &VersionedTransaction,
    ) -> Result<String> {
        let config = solana_client::rpc_config::RpcSendTransactionConfig {
            skip_preflight: true,
            preflight_commitment: Some(solana_sdk::commitment_config::CommitmentLevel::Processed),
            max_retries: Some(0),  // No retries - we want speed
            ..Default::default()
        };
        
        info!("📤 Sending transaction...");
        let sig = rpc_client.send_transaction_with_config(versioned_tx, config).await?;
        info!("✅ TX sent: {}", sig);
        
        // Fire and forget - don't wait for confirmation (adds 2-30 seconds latency)
        // User can check on Solscan if needed
        Ok(sig.to_string())
    }
    
    /// Start streaming transactions and take profit monitoring
    pub async fn stream_transactions(
        &self,
    ) -> Result<()> {
        // Spawn take profit monitor task (only if enabled and tiers are provided)
        if self.take_profit_enabled && !self.take_profit_tiers.is_empty() {
            let positions = self.positions.clone();
            let rpc_url = self.rpc_url.clone();
            let keypair = self.our_keypair.clone();
            let tip_amount = self.tip_amount;
            let tiers = self.take_profit_tiers.clone();

            tokio::spawn(async move {
                Self::take_profit_monitor(positions, rpc_url, keypair, tip_amount, tiers).await;
            });
        } else {
            info!("📈 Take Profit disabled (TAKE_PROFIT_ENABLED=false or no tiers)");
        }
        
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
        // Use PROCESSED for maximum speed - we react ASAP, don't wait for confirmation
        let (mut stream, _unsub) = pubsub_client
            .logs_subscribe(
                RpcTransactionLogsFilter::Mentions(vec![self.target_wallet.to_string()]),
                RpcTransactionLogsConfig {
                    commitment: Some(CommitmentConfig::processed()),
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
                    
                    // Extract token mint ONLY from logs - NEVER fetch transaction (adds 50-150ms latency)
                    let token_mint = self.extract_token_from_logs(&logs);
                    
                    if let Some(mint) = token_mint {
                        info!("🪙 Token mint: {}", mint);
                        
                        // Execute copy buy IMMEDIATELY - no delays
                        match self.execute_copy_buy(&mint, &signature).await {
                            Ok(our_sig) => {
                                info!("✅ COPY BUY EXECUTED! Sig: {}", our_sig);
                                // Add position for TP tracking
                                if self.take_profit_enabled && !self.take_profit_tiers.is_empty() {
                                    self.add_position(mint.clone(), self.buy_amount_sol, 0).await;
                                }
                            }
                            Err(e) => {
                                error!("❌ Copy buy failed: {:?}", e);
                            }
                        }
                    } else {
                        warn!("⚠️ Could not extract token mint from logs - skipping");
                    }
                }
                DetectedAction::Sell { signature, slot } => {
                    info!("🚨 TARGET SELL DETECTED! Signature: {} (slot: {})", signature, slot);
                    
                    // Extract token mint ONLY from logs - NEVER fetch transaction
                    let token_mint = self.extract_token_from_logs(&logs);
                    
                    if let Some(mint) = token_mint {
                        info!("🪙 Token being sold: {}", mint);
                        
                        // Execute copy sell - sell ALL our tokens of this mint
                        match self.execute_copy_sell(&mint, &signature).await {
                            Ok(our_sig) => {
                                info!("✅ COPY SELL EXECUTED! Our signature: {}", our_sig);
                                // Remove from tracked positions
                                self.remove_position(&mint).await;
                            }
                            Err(e) => {
                                // Check if it's just "no tokens" - that's not really an error
                                let err_str = format!("{:?}", e);
                                if err_str.contains("No tokens to sell") || err_str.contains("account not found") {
                                    info!("ℹ️ No tokens to sell for {} - probably already sold (TP or manually)", &mint[..8.min(mint.len())]);
                                    // Also remove from tracking
                                    self.remove_position(&mint).await;
                                } else {
                                    error!("❌ Copy sell failed: {:?}", e);
                                }
                            }
                        }
                    } else {
                        warn!("⚠️ Could not extract token mint from sell transaction");
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
    
    /// Get token balance checking both SPL Token and Token-2022 programs
    /// This is needed because Pump.fun tokens use Token-2022
    async fn get_token_balance_any_program(
        &self,
        rpc_client: &AsyncRpcClient,
        token_mint: &Pubkey,
    ) -> Result<(u64, u8)> {
        use solana_client::rpc_request::TokenAccountsFilter;
        
        let our_pubkey = self.our_keypair.pubkey();
        
        info!("🔍 Searching for token accounts for mint: {}", token_mint);
        
        // Use getTokenAccountsByOwner which works for both SPL Token and Token-2022
        // Filter by mint to find all accounts we own for this specific token
        let filter = TokenAccountsFilter::Mint(*token_mint);
        
        let accounts = rpc_client
            .get_token_accounts_by_owner(&our_pubkey, filter)
            .await;
        
        match accounts {
            Ok(token_accounts) => {
                info!("📋 Found {} token account(s) for this mint", token_accounts.len());

                let mut total_balance: u64 = 0;
                let mut decimals: Option<u8> = None;

                for account in token_accounts.iter() {
                    // Parse the account data to get balance
                    if let solana_account_decoder::UiAccountData::Json(parsed) = &account.account.data {
                        if let Some(info) = parsed.parsed.get("info") {
                            if let Some(token_amount) = info.get("tokenAmount") {
                                if decimals.is_none() {
                                    decimals = token_amount
                                        .get("decimals")
                                        .and_then(|v| v.as_u64())
                                        .and_then(|v| u8::try_from(v).ok());
                                }
                                if let Some(amount_str) = token_amount.get("amount") {
                                    if let Some(amount) = amount_str.as_str() {
                                        if let Ok(balance) = amount.parse::<u64>() {
                                            info!("  💰 Account {} has {} tokens", account.pubkey, balance);
                                            total_balance += balance;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                
                let decimals = decimals.unwrap_or(0);
                info!("💵 Total balance across all accounts: {} (decimals={})", total_balance, decimals);
                Ok((total_balance, decimals))
            }
            Err(e) => {
                warn!("⚠️ Error getting token accounts: {}", e);
                
                // Fallback: Try traditional ATA derivation for SPL Token
                let spl_ata = spl_associated_token_account::get_associated_token_address(
                    &our_pubkey,
                    token_mint,
                );
                
                info!("🔄 Trying SPL Token ATA: {}", spl_ata);
                
                if let Ok(balance) = rpc_client.get_token_account_balance(&spl_ata).await {
                    if let Ok(amount) = balance.amount.parse::<u64>() {
                        let decimals = balance.decimals as u8;
                        info!("✅ Found balance via SPL ATA: {} (decimals={})", amount, decimals);
                        return Ok((amount, decimals));
                    }
                }
                
                // Try Token-2022 ATA
                let token_2022_program = Pubkey::from_str("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")
                    .expect("Invalid Token-2022 program ID");
                
                let token_2022_ata = spl_associated_token_account::get_associated_token_address_with_program_id(
                    &our_pubkey,
                    token_mint,
                    &token_2022_program,
                );
                
                info!("🔄 Trying Token-2022 ATA: {}", token_2022_ata);
                
                if let Ok(balance) = rpc_client.get_token_account_balance(&token_2022_ata).await {
                    if let Ok(amount) = balance.amount.parse::<u64>() {
                        let decimals = balance.decimals as u8;
                        info!("✅ Found balance via Token-2022 ATA: {} (decimals={})", amount, decimals);
                        return Ok((amount, decimals));
                    }
                }
                
                info!("❌ No token accounts found for this mint");
                Ok((0, 0))
            }
        }
    }

    /// Check if a token is a Pump.fun token (ends with "pump")
    fn is_pumpfun_token(token_mint: &str) -> bool {
        token_mint.ends_with("pump")
    }
    
    /// Execute a copy buy transaction - uses Pump.fun API for pump tokens (faster!)
    async fn execute_copy_buy(&self, token_mint: &str, _target_signature: &str) -> Result<String> {
        let rpc_client = AsyncRpcClient::new(self.rpc_url.clone());
        
        let _token_mint_pubkey = Pubkey::from_str(token_mint)
            .context("Invalid token mint")?;
        
        info!("🔄 Building copy buy transaction for token: {}", token_mint);
        
        // Check if it's a pump.fun token - use their API for speed
        if Self::is_pumpfun_token(token_mint) {
            info!("🚀 Using PUMP.FUN API (faster for pump tokens)");
            return self.execute_pumpfun_buy(token_mint).await;
        }
        
        // Fallback to Jupiter for non-pump tokens
        info!("📊 Using Jupiter API (non-pump token)");
        self.execute_jupiter_buy(token_mint).await
    }
    
    /// Execute buy via Pump.fun API (fastest for pump tokens)
    async fn execute_pumpfun_buy(&self, token_mint: &str) -> Result<String> {
        let rpc_client = AsyncRpcClient::new(self.rpc_url.clone());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        
        // Pump.fun trade API - much faster than Jupiter for pump tokens
        // IMPORTANT: When denominatedInSol=true, amount is in SOL (not lamports!)
        let trade_request = serde_json::json!({
            "publicKey": self.our_keypair.pubkey().to_string(),
            "action": "buy",
            "mint": token_mint,
            "amount": self.buy_amount_sol,  // Amount in SOL when denominatedInSol=true
            "denominatedInSol": true,  // MUST be boolean, not string!
            "slippage": 50,  // 50% slippage for safety
            "priorityFee": self.tip_amount as f64 / 1_000_000_000.0,  // Convert to SOL
            "pool": "pump"
        });
        
        info!("⚡ Requesting Pump.fun BUY transaction...");
        let response = client.post(PUMPFUN_TRADE_API)
            .json(&trade_request)
            .send()
            .await
            .context("Failed to get Pump.fun trade")?;
        
        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            error!("Pump.fun API error ({}): {}", status, error_text);
            // Fallback to Jupiter if Pump.fun fails
            info!("⚠️ Falling back to Jupiter...");
            return self.execute_jupiter_buy(token_mint).await;
        }
        
        // The API returns the raw transaction bytes
        let tx_bytes = response.bytes().await
            .context("Failed to get transaction bytes")?;
        
        // Deserialize as versioned transaction
        let mut versioned_tx: VersionedTransaction = bincode::deserialize(&tx_bytes)
            .context("Failed to deserialize Pump.fun transaction")?;
        
        // Sign the transaction
        info!("✍️ Signing Pump.fun BUY transaction...");
        let message_bytes = versioned_tx.message.serialize();
        let signature = self.our_keypair.sign_message(&message_bytes);
        versioned_tx.signatures[0] = signature;
        
        // Send via RPC - fast, no waiting
        info!("📤 Sending BUY transaction...");
        let sig = self.send_transaction_fast(&rpc_client, &versioned_tx).await
            .context("Failed to send Pump.fun transaction")?;
        
        info!("🚀 Pump.fun BUY sent: {}", sig);
        Ok(sig)
    }
    
    /// Execute buy via Jupiter API (fallback for non-pump tokens)
    async fn execute_jupiter_buy(&self, token_mint: &str) -> Result<String> {
        let rpc_client = AsyncRpcClient::new(self.rpc_url.clone());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        
        let buy_amount_lamports = (self.buy_amount_sol * 1_000_000_000.0) as u64;
        
        // Step 1: Get quote from Jupiter
        let quote_url = format!(
            "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint={}&amount={}&slippageBps=2500",
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
            return Err(anyhow::anyhow!("Jupiter quote failed: {}", error_text));
        }
        
        let quote_data: serde_json::Value = quote_response.json().await
            .context("Failed to parse Jupiter quote")?;
        
        info!("✅ Got quote: {:?}", quote_data.get("outAmount"));
        
        // Step 2: Get swap transaction
        let swap_request = serde_json::json!({
            "quoteResponse": quote_data,
            "userPublicKey": self.our_keypair.pubkey().to_string(),
            "wrapAndUnwrapSol": true,
            "dynamicComputeUnitLimit": true,
            "prioritizationFeeLamports": self.tip_amount
        });
        
        info!("🔨 Building Jupiter swap...");
        let swap_response = client.post("https://quote-api.jup.ag/v6/swap")
            .json(&swap_request)
            .send()
            .await
            .context("Failed to get Jupiter swap")?;
        
        if !swap_response.status().is_success() {
            let error_text = swap_response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Jupiter swap failed: {}", error_text));
        }
        
        let swap_data: serde_json::Value = swap_response.json().await?;
        let swap_transaction = swap_data.get("swapTransaction")
            .and_then(|v| v.as_str())
            .context("No swap transaction")?;
        
        let tx_bytes = BASE64.decode(swap_transaction)?;
        let mut versioned_tx: VersionedTransaction = bincode::deserialize(&tx_bytes)?;
        
        // Sign
        let message_bytes = versioned_tx.message.serialize();
        let signature = self.our_keypair.sign_message(&message_bytes);
        versioned_tx.signatures[0] = signature;
        
        // Send via RPC - fast
        let sig = self.send_transaction_fast(&rpc_client, &versioned_tx).await?;
        info!("🚀 Jupiter BUY sent: {}", sig);
        
        Ok(sig)
    }
    
    /// Execute a copy sell transaction - uses Pump.fun API for pump tokens (faster!)
    async fn execute_copy_sell(&self, token_mint: &str, _target_signature: &str) -> Result<String> {
        let rpc_client = AsyncRpcClient::new(self.rpc_url.clone());
        
        info!("🔄 Preparing copy SELL for token: {}", token_mint);
        
        // First, get our token balance
        let token_mint_pubkey = Pubkey::from_str(token_mint)
            .context("Invalid token mint")?;
        
        // Try to get token balance + decimals (needed for PumpPortal SELL formatting)
        let (token_balance, token_decimals) = self
            .get_token_balance_any_program(&rpc_client, &token_mint_pubkey)
            .await?;

        info!(
            "💰 Our token balance: {} raw units (decimals={})",
            token_balance, token_decimals
        );

        if token_balance == 0 {
            return Err(anyhow::anyhow!("No tokens to sell - balance is 0"));
        }
        
        // Use Pump.fun API for pump tokens (faster!)
        if Self::is_pumpfun_token(token_mint) {
            info!("🚀 Using PUMP.FUN API for SELL (faster)");
            return self
                .execute_pumpfun_sell(token_mint, token_balance, token_decimals)
                .await;
        }
        
        // Fallback to Jupiter for non-pump tokens
        info!("📊 Using Jupiter API for SELL");
        self.execute_jupiter_sell(token_mint, token_balance).await
    }
    
    /// Execute sell via Pump.fun API (fastest for pump tokens)
    async fn execute_pumpfun_sell(&self, token_mint: &str, token_amount_raw: u64, token_decimals: u8) -> Result<String> {
        let rpc_client = AsyncRpcClient::new(self.rpc_url.clone());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        fn format_ui_amount(raw: u64, decimals: u8) -> String {
            if decimals == 0 {
                return raw.to_string();
            }
            let factor = 10u128.pow(decimals as u32);
            let raw_u = raw as u128;
            let whole = raw_u / factor;
            let frac = raw_u % factor;
            let mut frac_str = format!("{:0width$}", frac, width = decimals as usize);
            while frac_str.ends_with('0') {
                frac_str.pop();
            }
            if frac_str.is_empty() {
                whole.to_string()
            } else {
                format!("{}.{}", whole, frac_str)
            }
        }

        // PumpPortal SELL: many implementations expect amount in UI token units when denominatedInSol=false.
        let ui_amount = (token_amount_raw as f64) / 10f64.powi(token_decimals as i32);
        let ui_amount_str = format_ui_amount(token_amount_raw, token_decimals);
        info!(
            "🧮 SELL amounts: raw={} decimals={} ui={} ui_str={}",
            token_amount_raw, token_decimals, ui_amount, ui_amount_str
        );

        // We try a couple of encodings (Pump-only) because PumpPortal can be picky and returns generic 400.
        // 1) UI amount as exact decimal string
        // 2) UI amount as number (f64)
        // 3) raw base-units (u64)
        // 4) percentage string ("100%")
        let attempts: [serde_json::Value; 4] = [
            serde_json::json!(ui_amount_str),
            serde_json::json!(ui_amount),
            serde_json::json!(token_amount_raw),
            serde_json::json!("100%"),
        ];

        let attempts_len = attempts.len();
        let mut last_error: Option<anyhow::Error> = None;

        for (idx, amount_value) in attempts.into_iter().enumerate() {
            let trade_request = serde_json::json!({
                "publicKey": self.our_keypair.pubkey().to_string(),
                "action": "sell",
                "mint": token_mint,
                "amount": amount_value,
                "denominatedInSol": false,
                "slippage": 50,
                "priorityFee": self.tip_amount as f64 / 1_000_000_000.0,
                "pool": "pump"
            });

            info!(
                "⚡ Pump.fun SELL attempt {}/{} request: {}",
                idx + 1,
                attempts_len,
                serde_json::to_string_pretty(&trade_request).unwrap_or_default()
            );

            let response = client
                .post(PUMPFUN_TRADE_API)
                .json(&trade_request)
                .send()
                .await
                .context("Failed to get Pump.fun trade")?;

            if !response.status().is_success() {
                let status = response.status();
                let error_text = response.text().await.unwrap_or_default();
                let err = anyhow::anyhow!("Pump.fun SELL API error ({}): {}", status, error_text);
                last_error = Some(err);
                continue;
            }

            let tx_bytes = response
                .bytes()
                .await
                .context("Failed to get transaction bytes")?;

            let mut versioned_tx: VersionedTransaction = bincode::deserialize(&tx_bytes)
                .context("Failed to deserialize Pump.fun SELL transaction")?;

            info!("✍️ Signing Pump.fun SELL transaction...");
            let message_bytes = versioned_tx.message.serialize();
            let signature = self.our_keypair.sign_message(&message_bytes);
            versioned_tx.signatures[0] = signature;

            info!("📤 Sending SELL transaction...");
            let sig = self
                .send_transaction_fast(&rpc_client, &versioned_tx)
                .await
                .context("Failed to send Pump.fun SELL transaction")?;

            info!("🚀 Pump.fun SELL sent: {}", sig);
            return Ok(sig);
        }

        return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Pump.fun SELL failed")));
        
    }
    
    /// Execute sell via Jupiter API (fallback)
    async fn execute_jupiter_sell(&self, token_mint: &str, token_balance: u64) -> Result<String> {
        let rpc_client = AsyncRpcClient::new(self.rpc_url.clone());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        
        if token_balance == 0 {
            return Err(anyhow::anyhow!("No tokens to sell - balance is 0"));
        }
        
        // Step 1: Get quote (selling tokens for SOL)
        let quote_url = format!(
            "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint={}&amount={}&slippageBps=2500",
            token_mint,
            WSOL_MINT,
            token_balance
        );
        
        info!("📊 Getting Jupiter SELL quote for {} tokens...", token_balance);
        let quote_response = client.get(&quote_url)
            .send()
            .await
            .context("Failed to get Jupiter quote")?;
        
        if !quote_response.status().is_success() {
            let error_text = quote_response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Jupiter quote failed: {}", error_text));
        }
        
        let quote_data: serde_json::Value = quote_response.json().await?;
        let out_amount = quote_data.get("outAmount")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        
        info!("✅ Will receive ~{:.4} SOL", out_amount as f64 / 1_000_000_000.0);
        
        // Step 2: Get swap transaction
        let swap_request = serde_json::json!({
            "quoteResponse": quote_data,
            "userPublicKey": self.our_keypair.pubkey().to_string(),
            "wrapAndUnwrapSol": true,
            "dynamicComputeUnitLimit": true,
            "prioritizationFeeLamports": self.tip_amount
        });
        
        let swap_response = client.post("https://quote-api.jup.ag/v6/swap")
            .json(&swap_request)
            .send()
            .await?;
        
        if !swap_response.status().is_success() {
            let error_text = swap_response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Jupiter swap failed: {}", error_text));
        }
        
        let swap_data: serde_json::Value = swap_response.json().await?;
        let swap_transaction = swap_data.get("swapTransaction")
            .and_then(|v| v.as_str())
            .context("No swap transaction")?;
        
        let tx_bytes = BASE64.decode(swap_transaction)?;
        let mut versioned_tx: VersionedTransaction = bincode::deserialize(&tx_bytes)?;
        
        // Sign
        let message_bytes = versioned_tx.message.serialize();
        let signature = self.our_keypair.sign_message(&message_bytes);
        versioned_tx.signatures[0] = signature;
        
        // Send via RPC - fast
        let sig = self.send_transaction_fast(&rpc_client, &versioned_tx).await?;
        info!("🚀 Jupiter SELL sent: {}", sig);
        
        Ok(sig)
    }
    
    /// Take profit monitor - runs in background checking positions
    /// Uses Pump.fun bonding curve for price (faster than Jupiter)
    async fn take_profit_monitor(
        positions: Arc<RwLock<HashMap<String, PositionInfo>>>,
        rpc_url: String,
        keypair: Arc<Keypair>,
        tip_amount: u64,
        mut tiers: Vec<TakeProfitTier>,
    ) {
        // Sort tiers by multiplier asc (and keep them stable)
        tiers.sort_by(|a, b| a.multiplier.partial_cmp(&b.multiplier).unwrap_or(std::cmp::Ordering::Equal));
        info!(
            "📈 Take Profit Monitor started (tiers={})",
            serde_json::to_string(&tiers).unwrap_or_else(|_| "[]".to_string())
        );
        
        let rpc_client = AsyncRpcClient::new(rpc_url.clone());
        
        loop {
            // Check every 2 seconds - faster TP reaction
            sleep(Duration::from_secs(2)).await;
            
            let positions_to_check: Vec<PositionInfo> = {
                let positions_guard = positions.read().await;
                positions_guard.values().cloned().collect()
            };
            
            if positions_to_check.is_empty() {
                continue;
            }
            
            for position in positions_to_check {
                // Get token balance first
                let token_mint = match Pubkey::from_str(&position.token_mint) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                
                // Get current token balance + decimals
                let (token_balance, token_decimals) = match Self::get_token_balance_static(
                    &rpc_client,
                    &keypair.pubkey(),
                    &token_mint,
                )
                .await
                {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                
                if token_balance == 0 {
                    // Already sold, remove position
                    let mut positions_guard = positions.write().await;
                    positions_guard.remove(&position.token_mint);
                    continue;
                }
                
                // Get price from Pump.fun bonding curve (faster than Jupiter)
                match Self::get_pump_price(&position.token_mint).await {
                    Ok(price_sol) => {
                        let current_value = (token_balance as f64) * price_sol;
                        let profit_ratio = current_value / position.entry_sol;

                        // Find the highest tier that is reached and not yet applied
                        let mut selected: Option<&TakeProfitTier> = None;
                        for tier in tiers.iter() {
                            if profit_ratio >= tier.multiplier && tier.sell_percent > position.sold_percent {
                                selected = Some(tier);
                            }
                        }

                        if let Some(tier) = selected {
                            let target_percent = tier.sell_percent.min(100);
                            let already_percent = position.sold_percent.min(100);
                            if target_percent <= already_percent {
                                continue;
                            }

                            // Compute how many tokens to sell to reach target_percent cumulatively.
                            // We infer the initial token amount from remaining balance and already sold percent.
                            let current_balance_u128 = token_balance as u128;
                            let remaining_percent = 100u128.saturating_sub(already_percent as u128);
                            if remaining_percent == 0 {
                                // Shouldn't happen; treat as fully sold
                                let mut positions_guard = positions.write().await;
                                positions_guard.remove(&position.token_mint);
                                continue;
                            }
                            let initial_est = current_balance_u128
                                .saturating_mul(100u128)
                                .checked_div(remaining_percent)
                                .unwrap_or(current_balance_u128);

                            let delta_percent = (target_percent - already_percent) as u128;
                            let mut amount_to_sell = initial_est
                                .saturating_mul(delta_percent)
                                .checked_div(100u128)
                                .unwrap_or(0);

                            if amount_to_sell > current_balance_u128 {
                                amount_to_sell = current_balance_u128;
                            }
                            if amount_to_sell == 0 {
                                continue;
                            }

                            info!(
                                "🎯 TP {} reached: {:.2}x >= {:.2}x | sell {}% -> {}% (selling {} raw)",
                                &position.token_mint[..8],
                                profit_ratio,
                                tier.multiplier,
                                already_percent,
                                target_percent,
                                amount_to_sell
                            );

                            match Self::execute_take_profit_sell(
                                &position.token_mint,
                                amount_to_sell as u64,
                                token_decimals,
                                &rpc_url,
                                &keypair,
                                tip_amount,
                            )
                            .await
                            {
                                Ok(sig) => {
                                    info!("✅ TP SELL: {}", sig);
                                    let mut positions_guard = positions.write().await;
                                    if target_percent >= 100 {
                                        positions_guard.remove(&position.token_mint);
                                    } else if let Some(p) = positions_guard.get_mut(&position.token_mint) {
                                        p.sold_percent = target_percent;
                                    }
                                }
                                Err(e) => {
                                    error!("❌ TP sell failed: {:?}", e);
                                }
                            }
                        }
                    }
                    Err(_) => {}
                }
            }
        }
    }
    
    /// Get token balance (static version for TP monitor)
    async fn get_token_balance_static(
        rpc_client: &AsyncRpcClient,
        owner: &Pubkey,
        token_mint: &Pubkey,
    ) -> Result<(u64, u8)> {
        use solana_client::rpc_request::TokenAccountsFilter;
        
        let filter = TokenAccountsFilter::Mint(*token_mint);
        let accounts = rpc_client.get_token_accounts_by_owner(owner, filter).await?;
        
        let mut total: u64 = 0;
        let mut decimals: Option<u8> = None;
        for account in accounts.iter() {
            if let solana_account_decoder::UiAccountData::Json(parsed) = &account.account.data {
                if let Some(info) = parsed.parsed.get("info") {
                    if let Some(token_amount) = info.get("tokenAmount") {
                        if decimals.is_none() {
                            decimals = token_amount
                                .get("decimals")
                                .and_then(|v| v.as_u64())
                                .and_then(|v| u8::try_from(v).ok());
                        }
                        if let Some(amount_str) = token_amount.get("amount") {
                            if let Some(amount) = amount_str.as_str() {
                                total += amount.parse::<u64>().unwrap_or(0);
                            }
                        }
                    }
                }
            }
        }
        Ok((total, decimals.unwrap_or(0)))
    }
    
    /// Get price from Pump.fun bonding curve (much faster than Jupiter)
    async fn get_pump_price(token_mint: &str) -> Result<f64> {
        let client = reqwest::Client::new();
        
        // Pump.fun quote API - gives us the bonding curve price
        let url = format!("https://pumpportal.fun/api/quote?mint={}&sol=0.001&isBuy=true", token_mint);
        
        let response = client.get(&url)
            .timeout(Duration::from_secs(3))
            .send()
            .await?;
        
        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Quote failed"));
        }
        
        let data: serde_json::Value = response.json().await?;
        
        // tokens_out for 0.001 SOL tells us the price
        // price = 0.001 / tokens_out
        if let Some(tokens_out) = data.get("tokensOut").and_then(|v| v.as_f64()) {
            if tokens_out > 0.0 {
                return Ok(0.001 / tokens_out);
            }
        }
        
        Err(anyhow::anyhow!("Could not parse price"))
    }
    
    /// Execute take profit sell - uses Pump.fun for pump tokens
    async fn execute_take_profit_sell(
        token_mint: &str,
        token_amount: u64,
        token_decimals: u8,
        rpc_url: &str,
        keypair: &Arc<Keypair>,
        tip_amount: u64,
    ) -> Result<String> {
        let rpc_client = AsyncRpcClient::new(rpc_url.to_string());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        
        // Use Pump.fun API for pump tokens (faster!)
        if token_mint.ends_with("pump") {
            fn format_ui_amount(raw: u64, decimals: u8) -> String {
                if decimals == 0 {
                    return raw.to_string();
                }
                let factor = 10u128.pow(decimals as u32);
                let raw_u = raw as u128;
                let whole = raw_u / factor;
                let frac = raw_u % factor;
                let mut frac_str = format!("{:0width$}", frac, width = decimals as usize);
                while frac_str.ends_with('0') {
                    frac_str.pop();
                }
                if frac_str.is_empty() {
                    whole.to_string()
                } else {
                    format!("{}.{}", whole, frac_str)
                }
            }

            let ui_amount = (token_amount as f64) / 10f64.powi(token_decimals as i32);
            let ui_amount_str = format_ui_amount(token_amount, token_decimals);
            info!(
                "⚡ Pump.fun TP SELL raw={} decimals={} ui={} ui_str={}",
                token_amount, token_decimals, ui_amount, ui_amount_str
            );

            // Pump-only: try a few encodings because PumpPortal can return generic 400.
            let attempts: [serde_json::Value; 4] = [
                serde_json::json!(ui_amount_str),
                serde_json::json!(ui_amount),
                serde_json::json!(token_amount),
                serde_json::json!("100%"),
            ];

            let attempts_len = attempts.len();
            let mut last_error: Option<anyhow::Error> = None;
            for (idx, amount_value) in attempts.into_iter().enumerate() {
                let trade_request = serde_json::json!({
                    "publicKey": keypair.pubkey().to_string(),
                    "action": "sell",
                    "mint": token_mint,
                    "amount": amount_value,
                    "denominatedInSol": false,
                    "slippage": 50,
                    "priorityFee": tip_amount as f64 / 1_000_000_000.0,
                    "pool": "pump"
                });

                info!(
                    "⚡ Pump.fun TP SELL attempt {}/{} request: {}",
                    idx + 1,
                    attempts_len,
                    serde_json::to_string_pretty(&trade_request).unwrap_or_default()
                );

                let response = client.post(PUMPFUN_TRADE_API).json(&trade_request).send().await?;
                if !response.status().is_success() {
                    let status = response.status();
                    let error_text = response.text().await.unwrap_or_default();
                    last_error = Some(anyhow::anyhow!("Pump.fun TP SELL API error ({}): {}", status, error_text));
                    continue;
                }

                let tx_bytes = response.bytes().await?;
                let mut versioned_tx: VersionedTransaction = bincode::deserialize(&tx_bytes)?;

                let message_bytes = versioned_tx.message.serialize();
                let signature = keypair.sign_message(&message_bytes);
                versioned_tx.signatures[0] = signature;

                let config = solana_client::rpc_config::RpcSendTransactionConfig {
                    skip_preflight: true,
                    preflight_commitment: Some(solana_sdk::commitment_config::CommitmentLevel::Processed),
                    max_retries: Some(0),
                    ..Default::default()
                };

                let sig = rpc_client.send_transaction_with_config(&versioned_tx, config).await?;
                return Ok(sig.to_string());
            }

            return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Pump.fun TP SELL failed")));
        }
        
        // Jupiter fallback
        let quote_url = format!(
            "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint={}&amount={}&slippageBps=2500",
            token_mint,
            WSOL_MINT,
            token_amount
        );
        
        let quote_response = client.get(&quote_url).send().await?;
        let quote_data: serde_json::Value = quote_response.json().await?;
        
        let swap_request = serde_json::json!({
            "quoteResponse": quote_data,
            "userPublicKey": keypair.pubkey().to_string(),
            "wrapAndUnwrapSol": true,
            "dynamicComputeUnitLimit": true,
            "prioritizationFeeLamports": tip_amount
        });
        
        let swap_response = client.post("https://quote-api.jup.ag/v6/swap")
            .json(&swap_request)
            .send()
            .await?;
        let swap_data: serde_json::Value = swap_response.json().await?;
        
        let swap_transaction = swap_data.get("swapTransaction")
            .and_then(|v| v.as_str())
            .context("No swap transaction")?;
        
        let tx_bytes = BASE64.decode(swap_transaction)?;
        let mut versioned_tx: VersionedTransaction = bincode::deserialize(&tx_bytes)?;
        
        let message_bytes = versioned_tx.message.serialize();
        let signature = keypair.sign_message(&message_bytes);
        versioned_tx.signatures[0] = signature;
        
        let config = solana_client::rpc_config::RpcSendTransactionConfig {
            skip_preflight: true,
            preflight_commitment: Some(solana_sdk::commitment_config::CommitmentLevel::Confirmed),
            max_retries: Some(3),
            ..Default::default()
        };
        
        let sig = rpc_client.send_transaction_with_config(&versioned_tx, config).await?;
        Ok(sig.to_string())
    }
    
    /// Add a position to track for take profit
    pub async fn add_position(&self, token_mint: String, entry_sol: f64, token_amount: u64) {
        let position = PositionInfo {
            token_mint: token_mint.clone(),
            entry_sol,
            token_amount,
            entry_time: std::time::Instant::now(),
            sold_percent: 0,
        };
        
        let mut positions = self.positions.write().await;
        positions.insert(token_mint.clone(), position);
        info!(
            "📝 Position added: {} ({:.4} SOL for {} tokens, sold=0%)",
            &token_mint[..8],
            entry_sol,
            token_amount
        );
    }
    
    /// Remove a position (when sold)
    pub async fn remove_position(&self, token_mint: &str) {
        let mut positions = self.positions.write().await;
        if positions.remove(token_mint).is_some() {
            info!("📝 Position removed: {}", &token_mint[..8]);
        }
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
    take_profit_enabled: bool,
    take_profit_tiers: Vec<TakeProfitTier>,
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
            take_profit_enabled: true,
            take_profit_tiers: vec![
                TakeProfitTier { multiplier: 2.0, sell_percent: 100 },
            ],
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

    pub fn take_profit_enabled(mut self, enabled: bool) -> Self {
        self.take_profit_enabled = enabled;
        self
    }

    pub fn take_profit_tiers(mut self, tiers: Vec<TakeProfitTier>) -> Self {
        self.take_profit_tiers = tiers;
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
            self.take_profit_enabled,
            self.take_profit_tiers,
        ))
    }
}
