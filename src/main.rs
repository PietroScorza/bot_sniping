//! Solana Copytrading Bot
//! 
//! High-performance copytrading bot using Helius Yellowstone gRPC for 
//! transaction monitoring and Jito bundles for MEV-protected execution.

use anyhow::Result;
use solana_sdk::signer::Signer;
use tracing::{info, error};
use tracing_subscriber::{fmt, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use tokio::signal;
use std::sync::Arc;

mod config;
mod grpc;
mod decoder;
mod jito;
mod state;
mod engine;

use config::Config;
use engine::TradingEngine;
use state::StateManager;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    init_logging();
    
    info!("🚀 Starting Solana Copytrading Bot...");
    
    // Load configuration
    let (config, keypair) = Config::from_env()?;
    let keypair = Arc::new(keypair);
    
    info!("✅ Configuration loaded successfully");
    info!("📍 Target wallet: {}", config.target_wallet);
    info!("💰 Buy amount: {} SOL", config.buy_amount_sol);
    info!("🔑 Our wallet: {}", keypair.pubkey());
    
    // Initialize state manager
    let state = Arc::new(StateManager::new());
    info!("✅ State manager initialized");
    
    // Initialize trading engine
    let engine = TradingEngine::new(&config, keypair.clone(), state.clone()).await?;
    info!("✅ Trading engine initialized");
    
    // Start the engine in a separate task
    let engine_handle = tokio::spawn(async move {
        if let Err(e) = engine.run().await {
            error!("Trading engine error: {:?}", e);
        }
    });
    
    // Wait for shutdown signal
    info!("🎯 Bot is running. Press Ctrl+C to stop.");
    shutdown_signal().await;
    
    info!("🛑 Shutdown signal received, stopping bot...");
    
    // Cleanup
    engine_handle.abort();
    
    info!("👋 Bot stopped gracefully");
    Ok(())
}

/// Initialize the logging system
fn init_logging() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,solana_copytrading_bot=debug"));
    
    let json_logging = std::env::var("LOG_JSON")
        .map(|v| v == "true")
        .unwrap_or(false);
    
    if json_logging {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer()
                .with_target(true)
                .with_thread_ids(true)
                .with_file(true)
                .with_line_number(true))
            .init();
    }
}

/// Wait for shutdown signal (Ctrl+C or SIGTERM)
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
