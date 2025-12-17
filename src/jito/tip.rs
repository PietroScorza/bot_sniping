//! Jito tip account management

use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use rand::seq::SliceRandom;

/// Jito tip accounts for bundle submission
/// These are the official Jito tip accounts on mainnet
pub const JITO_TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4bVqkfRtRhsMVYH4bM2vKW1",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];

/// Get a random tip account for better distribution
pub fn get_random_tip_account() -> Pubkey {
    let account = JITO_TIP_ACCOUNTS.choose(&mut rand::thread_rng())
        .expect("Tip accounts should not be empty");
    Pubkey::from_str(account).expect("Invalid tip account")
}

/// Get all tip accounts as Pubkeys
pub fn get_all_tip_accounts() -> Vec<Pubkey> {
    JITO_TIP_ACCOUNTS.iter()
        .filter_map(|s| Pubkey::from_str(s).ok())
        .collect()
}

/// Tip priority level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipLevel {
    /// Normal tip for non-urgent transactions (Take Profit)
    Normal,
    /// Emergency tip for urgent transactions (Copy Sell)
    Emergency,
    /// Custom tip amount
    Custom(u64),
}

impl TipLevel {
    /// Get the tip amount in lamports based on configuration
    pub fn get_amount(&self, normal_tip: u64, emergency_tip: u64) -> u64 {
        match self {
            TipLevel::Normal => normal_tip,
            TipLevel::Emergency => emergency_tip,
            TipLevel::Custom(amount) => *amount,
        }
    }
}

/// Tip configuration
#[derive(Debug, Clone)]
pub struct TipConfig {
    pub normal_amount: u64,
    pub emergency_amount: u64,
    pub max_amount: u64,
}

impl TipConfig {
    pub fn new(normal: u64, emergency: u64, max: u64) -> Self {
        Self {
            normal_amount: normal,
            emergency_amount: emergency,
            max_amount: max,
        }
    }
    
    /// Get tip amount with safety cap
    pub fn get_tip(&self, level: TipLevel) -> u64 {
        let amount = level.get_amount(self.normal_amount, self.emergency_amount);
        amount.min(self.max_amount)
    }
}

impl Default for TipConfig {
    fn default() -> Self {
        Self {
            normal_amount: 10_000,      // 0.00001 SOL
            emergency_amount: 100_000,   // 0.0001 SOL
            max_amount: 500_000,         // 0.0005 SOL
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_tip_accounts() {
        let accounts = get_all_tip_accounts();
        assert_eq!(accounts.len(), 8);
    }
    
    #[test]
    fn test_random_tip_account() {
        let account = get_random_tip_account();
        let accounts = get_all_tip_accounts();
        assert!(accounts.contains(&account));
    }
    
    #[test]
    fn test_tip_config() {
        let config = TipConfig::new(10_000, 100_000, 500_000);
        assert_eq!(config.get_tip(TipLevel::Normal), 10_000);
        assert_eq!(config.get_tip(TipLevel::Emergency), 100_000);
        assert_eq!(config.get_tip(TipLevel::Custom(200_000)), 200_000);
        // Test cap
        assert_eq!(config.get_tip(TipLevel::Custom(1_000_000)), 500_000);
    }
}
