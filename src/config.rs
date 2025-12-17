//! Configuration module for the copytrading bot

use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use std::str::FromStr;

/// Take profit tier configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakeProfitTier {
    /// Price multiplier to trigger (e.g., 2.0 = 2x)
    pub multiplier: f64,
    /// Percentage of position to sell (0-100)
    pub sell_percent: u8,
}

/// Main configuration structure
#[derive(Debug)]
pub struct Config {
    // Wallet configuration
    pub keypair: Keypair,
    pub target_wallet: Pubkey,
    
    // Helius configuration
    pub helius_grpc_url: String,
    pub helius_api_key: String,
    pub solana_rpc_url: String,
    
    // Jito configuration
    pub jito_block_engine_url: String,
    pub jito_grpc_url: String,
    pub tip_amount_normal: u64,
    pub tip_amount_emergency: u64,
    pub tip_amount_max: u64,
    
    // Trading configuration
    pub buy_amount_sol: f64,
    pub buy_amount_proportional: f64,
    pub max_buy_amount_sol: f64,
    pub slippage_bps: u16,
    
    // Take profit configuration
    pub take_profit_enabled: bool,
    pub take_profit_tiers: Vec<TakeProfitTier>,
    
    // DEX program IDs
    pub raydium_amm_program: Pubkey,
    pub raydium_clmm_program: Pubkey,
    pub jupiter_program: Pubkey,
    pub pumpfun_program: Pubkey,
    pub orca_whirlpool_program: Pubkey,
    
    // Performance settings
    pub reconnect_delay_ms: u64,
    pub max_reconnect_attempts: u32,
    pub tx_confirmation_timeout_ms: u64,
    pub compute_unit_limit: u32,
    pub priority_fee_micro_lamports: u64,
}

impl Config {
    /// Load configuration from environment variables
    /// Returns (Config, Keypair) tuple since Keypair is not Clone
    pub fn from_env() -> Result<(Self, Keypair)> {
        dotenvy::dotenv().ok();
        
        // Parse private key
        let private_key_str = std::env::var("PRIVATE_KEY")
            .context("PRIVATE_KEY not set")?;
        let keypair = parse_keypair(&private_key_str)
            .context("Failed to parse PRIVATE_KEY")?;
        
        // Create a second keypair for the config (from same key)
        let config_keypair = parse_keypair(&private_key_str)
            .context("Failed to parse PRIVATE_KEY for config")?;
        
        // Parse target wallet
        let target_wallet = Pubkey::from_str(
            &std::env::var("TARGET_WALLET").context("TARGET_WALLET not set")?
        ).context("Invalid TARGET_WALLET")?;
        
        // Parse take profit tiers
        let take_profit_tiers: Vec<TakeProfitTier> = std::env::var("TAKE_PROFIT_TIERS")
            .map(|s| serde_json::from_str(&s).unwrap_or_default())
            .unwrap_or_else(|_| vec![
                TakeProfitTier { multiplier: 2.0, sell_percent: 20 },
                TakeProfitTier { multiplier: 3.0, sell_percent: 30 },
                TakeProfitTier { multiplier: 5.0, sell_percent: 50 },
            ]);
        
        let config = Config {
            keypair,
            target_wallet,
            
            // Helius
            helius_grpc_url: std::env::var("HELIUS_GRPC_URL")
                .unwrap_or_else(|_| "https://atlas-mainnet.helius-rpc.com".to_string()),
            helius_api_key: std::env::var("HELIUS_API_KEY")
                .context("HELIUS_API_KEY not set")?,
            solana_rpc_url: std::env::var("SOLANA_RPC_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
            
            // Jito
            jito_block_engine_url: std::env::var("JITO_BLOCK_ENGINE_URL")
                .unwrap_or_else(|_| "https://frankfurt.mainnet.block-engine.jito.wtf".to_string()),
            jito_grpc_url: std::env::var("JITO_GRPC_URL")
                .unwrap_or_else(|_| "https://frankfurt.mainnet.block-engine.jito.wtf:443".to_string()),
            tip_amount_normal: std::env::var("TIP_AMOUNT_NORMAL")
                .unwrap_or_else(|_| "10000".to_string())
                .parse()
                .unwrap_or(10_000),
            tip_amount_emergency: std::env::var("TIP_AMOUNT_EMERGENCY")
                .unwrap_or_else(|_| "100000".to_string())
                .parse()
                .unwrap_or(100_000),
            tip_amount_max: std::env::var("TIP_AMOUNT_MAX")
                .unwrap_or_else(|_| "500000".to_string())
                .parse()
                .unwrap_or(500_000),
            
            // Trading
            buy_amount_sol: std::env::var("BUY_AMOUNT_SOL")
                .unwrap_or_else(|_| "0.1".to_string())
                .parse()
                .unwrap_or(0.1),
            buy_amount_proportional: std::env::var("BUY_AMOUNT_PROPORTIONAL")
                .unwrap_or_else(|_| "0".to_string())
                .parse()
                .unwrap_or(0.0),
            max_buy_amount_sol: std::env::var("MAX_BUY_AMOUNT_SOL")
                .unwrap_or_else(|_| "1.0".to_string())
                .parse()
                .unwrap_or(1.0),
            slippage_bps: std::env::var("SLIPPAGE_BPS")
                .unwrap_or_else(|_| "500".to_string())
                .parse()
                .unwrap_or(500),
            
            // Take profit
            take_profit_enabled: std::env::var("TAKE_PROFIT_ENABLED")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .unwrap_or(true),
            take_profit_tiers,
            
            // DEX programs
            raydium_amm_program: Pubkey::from_str(
                &std::env::var("RAYDIUM_AMM_PROGRAM")
                    .unwrap_or_else(|_| "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string())
            ).unwrap(),
            raydium_clmm_program: Pubkey::from_str(
                &std::env::var("RAYDIUM_CLMM_PROGRAM")
                    .unwrap_or_else(|_| "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK".to_string())
            ).unwrap(),
            jupiter_program: Pubkey::from_str(
                &std::env::var("JUPITER_PROGRAM")
                    .unwrap_or_else(|_| "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4".to_string())
            ).unwrap(),
            pumpfun_program: Pubkey::from_str(
                &std::env::var("PUMPFUN_PROGRAM")
                    .unwrap_or_else(|_| "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P".to_string())
            ).unwrap(),
            orca_whirlpool_program: Pubkey::from_str(
                &std::env::var("ORCA_WHIRLPOOL_PROGRAM")
                    .unwrap_or_else(|_| "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc".to_string())
            ).unwrap(),
            
            // Performance
            reconnect_delay_ms: std::env::var("RECONNECT_DELAY_MS")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .unwrap_or(1000),
            max_reconnect_attempts: std::env::var("MAX_RECONNECT_ATTEMPTS")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .unwrap_or(10),
            tx_confirmation_timeout_ms: std::env::var("TX_CONFIRMATION_TIMEOUT_MS")
                .unwrap_or_else(|_| "30000".to_string())
                .parse()
                .unwrap_or(30_000),
            compute_unit_limit: std::env::var("COMPUTE_UNIT_LIMIT")
                .unwrap_or_else(|_| "400000".to_string())
                .parse()
                .unwrap_or(400_000),
            priority_fee_micro_lamports: std::env::var("PRIORITY_FEE_MICRO_LAMPORTS")
                .unwrap_or_else(|_| "10000".to_string())
                .parse()
                .unwrap_or(10_000),
        };
        
        Ok((config, config_keypair))
    }
    
    /// Get the bot's public key
    pub fn pubkey(&self) -> Pubkey {
        self.keypair.pubkey()
    }
    
    /// Check if a program ID is a known DEX
    pub fn is_dex_program(&self, program_id: &Pubkey) -> bool {
        *program_id == self.raydium_amm_program
            || *program_id == self.raydium_clmm_program
            || *program_id == self.jupiter_program
            || *program_id == self.pumpfun_program
            || *program_id == self.orca_whirlpool_program
    }
}

/// Parse a keypair from various formats (base58, JSON array)
fn parse_keypair(input: &str) -> Result<Keypair> {
    // Try base58 first
    if let Ok(bytes) = bs58::decode(input.trim()).into_vec() {
        if bytes.len() == 64 {
            return Ok(Keypair::from_bytes(&bytes)?);
        }
    }
    
    // Try JSON array format
    if input.trim().starts_with('[') {
        let bytes: Vec<u8> = serde_json::from_str(input)?;
        if bytes.len() == 64 {
            return Ok(Keypair::from_bytes(&bytes)?);
        }
    }
    
    anyhow::bail!("Invalid keypair format. Expected base58 or JSON array of 64 bytes.");
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_default_take_profit_tiers() {
        let tiers = vec![
            TakeProfitTier { multiplier: 2.0, sell_percent: 20 },
            TakeProfitTier { multiplier: 3.0, sell_percent: 30 },
        ];
        
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].multiplier, 2.0);
        assert_eq!(tiers[0].sell_percent, 20);
    }
}
