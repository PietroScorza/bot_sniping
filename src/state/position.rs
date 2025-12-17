//! Position tracking structures

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use std::collections::HashSet;

/// Represents an open trading position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    /// Token mint address
    pub token_mint: Pubkey,
    /// Amount of tokens held
    pub amount: u64,
    /// Entry price in SOL (lamports per token)
    pub entry_price: f64,
    /// Total SOL invested (in lamports)
    pub invested_sol: u64,
    /// When the position was opened
    pub opened_at: DateTime<Utc>,
    /// Current value in SOL (updated periodically)
    pub current_value_sol: u64,
    /// Take profit tiers already triggered
    pub triggered_tp_tiers: HashSet<usize>,
    /// Original buy signature from target
    pub target_buy_signature: String,
    /// Our buy transaction signature
    pub our_buy_signature: String,
}

impl Position {
    /// Create a new position
    pub fn new(
        token_mint: Pubkey,
        amount: u64,
        invested_sol: u64,
        target_buy_signature: String,
        our_buy_signature: String,
    ) -> Self {
        let entry_price = if amount > 0 {
            invested_sol as f64 / amount as f64
        } else {
            0.0
        };
        
        Self {
            token_mint,
            amount,
            entry_price,
            invested_sol,
            opened_at: Utc::now(),
            current_value_sol: invested_sol,
            triggered_tp_tiers: HashSet::new(),
            target_buy_signature,
            our_buy_signature,
        }
    }
    
    /// Calculate current profit multiplier
    pub fn profit_multiplier(&self) -> f64 {
        if self.invested_sol == 0 {
            return 1.0;
        }
        self.current_value_sol as f64 / self.invested_sol as f64
    }
    
    /// Calculate absolute profit in lamports
    pub fn profit_lamports(&self) -> i64 {
        self.current_value_sol as i64 - self.invested_sol as i64
    }
    
    /// Calculate profit percentage
    pub fn profit_percent(&self) -> f64 {
        (self.profit_multiplier() - 1.0) * 100.0
    }
    
    /// Update current value
    pub fn update_value(&mut self, new_value_sol: u64) {
        self.current_value_sol = new_value_sol;
    }
    
    /// Reduce position (partial sell)
    pub fn reduce(&mut self, amount_sold: u64, sol_received: u64) {
        if amount_sold >= self.amount {
            self.amount = 0;
        } else {
            self.amount -= amount_sold;
        }
        
        // Reduce invested proportionally
        let sold_ratio = amount_sold as f64 / (self.amount + amount_sold) as f64;
        let invested_reduction = (self.invested_sol as f64 * sold_ratio) as u64;
        self.invested_sol = self.invested_sol.saturating_sub(invested_reduction);
    }
    
    /// Check if position is closed (zero amount)
    pub fn is_closed(&self) -> bool {
        self.amount == 0
    }
    
    /// Mark a take profit tier as triggered
    pub fn mark_tp_triggered(&mut self, tier_index: usize) {
        self.triggered_tp_tiers.insert(tier_index);
    }
    
    /// Check if a take profit tier has been triggered
    pub fn is_tp_triggered(&self, tier_index: usize) -> bool {
        self.triggered_tp_tiers.contains(&tier_index)
    }
}

/// Trade history entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    /// Trade ID
    pub id: uuid::Uuid,
    /// Token mint
    pub token_mint: Pubkey,
    /// Trade type
    pub trade_type: TradeRecordType,
    /// Amount traded
    pub amount: u64,
    /// SOL amount (in/out)
    pub sol_amount: u64,
    /// Transaction signature
    pub signature: String,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
    /// Profit/Loss for sells (None for buys)
    pub pnl: Option<i64>,
}

/// Type of trade record
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TradeRecordType {
    Buy,
    SellTakeProfit,
    SellCopyExit,
    SellManual,
}

impl TradeRecord {
    /// Create a new buy record
    pub fn new_buy(
        token_mint: Pubkey,
        amount: u64,
        sol_amount: u64,
        signature: String,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            token_mint,
            trade_type: TradeRecordType::Buy,
            amount,
            sol_amount,
            signature,
            timestamp: Utc::now(),
            pnl: None,
        }
    }
    
    /// Create a new sell record
    pub fn new_sell(
        token_mint: Pubkey,
        trade_type: TradeRecordType,
        amount: u64,
        sol_amount: u64,
        signature: String,
        pnl: Option<i64>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            token_mint,
            trade_type,
            amount,
            sol_amount,
            signature,
            timestamp: Utc::now(),
            pnl,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_position_creation() {
        let mint = Pubkey::new_unique();
        let position = Position::new(
            mint,
            1_000_000,
            100_000_000, // 0.1 SOL
            "target_sig".to_string(),
            "our_sig".to_string(),
        );
        
        assert_eq!(position.token_mint, mint);
        assert_eq!(position.amount, 1_000_000);
        assert_eq!(position.invested_sol, 100_000_000);
        assert!(!position.is_closed());
    }
    
    #[test]
    fn test_profit_calculation() {
        let mint = Pubkey::new_unique();
        let mut position = Position::new(
            mint,
            1_000_000,
            100_000_000,
            "target_sig".to_string(),
            "our_sig".to_string(),
        );
        
        // 2x profit
        position.update_value(200_000_000);
        assert!((position.profit_multiplier() - 2.0).abs() < 0.001);
        assert!((position.profit_percent() - 100.0).abs() < 0.1);
    }
    
    #[test]
    fn test_position_reduction() {
        let mint = Pubkey::new_unique();
        let mut position = Position::new(
            mint,
            1_000_000,
            100_000_000,
            "target_sig".to_string(),
            "our_sig".to_string(),
        );
        
        position.reduce(500_000, 75_000_000);
        assert_eq!(position.amount, 500_000);
    }
}
