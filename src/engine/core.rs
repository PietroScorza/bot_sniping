//! Core trading engine implementation

use anyhow::Result;
use solana_sdk::signature::Keypair;
use std::sync::Arc;
use tracing::info;

use crate::config::Config;
use crate::grpc::{HeliusGrpcClient, HeliusClientBuilder};
use crate::state::StateManager;

/// Core trading engine that orchestrates the copytrading logic
pub struct TradingEngine {
    helius_client: HeliusGrpcClient,
}

impl TradingEngine {
    /// Create a new trading engine
    pub async fn new(config: &Config, keypair: Arc<Keypair>, _state: Arc<StateManager>) -> Result<Self> {
        // Build Helius WebSocket client with trading parameters
        let helius_client = HeliusClientBuilder::new()
            .endpoint(&config.helius_grpc_url)
            .api_key(&config.helius_api_key)
            .target_wallet(config.target_wallet)
            .keypair(keypair)
            .buy_amount_sol(config.buy_amount_sol)
            .tip_amount(config.tip_amount_normal)
            .reconnect_delay_ms(config.reconnect_delay_ms)
            .max_reconnect_attempts(config.max_reconnect_attempts)
            .build()?;
        
        Ok(Self {
            helius_client,
        })
    }
    
    /// Run the trading engine
    pub async fn run(self) -> Result<()> {
        info!("🚀 Starting trading engine...");
        info!("📡 Streaming transactions via Helius WebSocket...");
        
        // Start streaming (this runs the loop internally)
        self.helius_client.stream_transactions().await
    }
}

