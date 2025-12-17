//! State manager for tracking positions and trade history

use dashmap::DashMap;
use parking_lot::RwLock;
use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{info, debug, warn};

use super::position::{Position, TradeRecord, TradeRecordType};

/// Thread-safe state manager
pub struct StateManager {
    /// Open positions indexed by token mint
    positions: DashMap<Pubkey, Position>,
    /// Trade history
    trade_history: RwLock<Vec<TradeRecord>>,
    /// Tokens we've already traded (to avoid duplicate buys)
    traded_tokens: DashMap<Pubkey, ()>,
    /// Pending transactions (signature -> token mint)
    pending_txs: DashMap<String, Pubkey>,
}

impl StateManager {
    /// Create a new state manager
    pub fn new() -> Self {
        Self {
            positions: DashMap::new(),
            trade_history: RwLock::new(Vec::new()),
            traded_tokens: DashMap::new(),
            pending_txs: DashMap::new(),
        }
    }
    
    /// Check if we've already traded a token
    pub fn has_traded_token(&self, token_mint: &Pubkey) -> bool {
        self.traded_tokens.contains_key(token_mint)
    }
    
    /// Check if we have an open position for a token
    pub fn has_position(&self, token_mint: &Pubkey) -> bool {
        self.positions.get(token_mint)
            .map(|p| !p.is_closed())
            .unwrap_or(false)
    }
    
    /// Check if we can copy-buy a token (first buy only)
    pub fn can_copy_buy(&self, token_mint: &Pubkey) -> bool {
        !self.has_traded_token(token_mint) && !self.has_position(token_mint)
    }
    
    /// Open a new position
    pub fn open_position(&self, position: Position) {
        let token_mint = position.token_mint;
        
        // Mark token as traded
        self.traded_tokens.insert(token_mint, ());
        
        // Add trade record
        let record = TradeRecord::new_buy(
            position.token_mint,
            position.amount,
            position.invested_sol,
            position.our_buy_signature.clone(),
        );
        self.trade_history.write().push(record);
        
        // Store position
        self.positions.insert(token_mint, position);
        
        info!("📈 Opened position for token: {}", token_mint);
    }
    
    /// Get a position by token mint
    pub fn get_position(&self, token_mint: &Pubkey) -> Option<Position> {
        self.positions.get(token_mint).map(|p| p.clone())
    }
    
    /// Update a position's current value
    pub fn update_position_value(&self, token_mint: &Pubkey, new_value_sol: u64) {
        if let Some(mut position) = self.positions.get_mut(token_mint) {
            position.update_value(new_value_sol);
        }
    }
    
    /// Reduce a position (partial sell)
    pub fn reduce_position(
        &self,
        token_mint: &Pubkey,
        amount_sold: u64,
        sol_received: u64,
        trade_type: TradeRecordType,
        signature: String,
    ) -> Option<Position> {
        let mut result = None;
        
        if let Some(mut position) = self.positions.get_mut(token_mint) {
            let pnl = if position.invested_sol > 0 {
                let invested_ratio = amount_sold as f64 / position.amount as f64;
                let invested_portion = (position.invested_sol as f64 * invested_ratio) as i64;
                Some(sol_received as i64 - invested_portion)
            } else {
                None
            };
            
            position.reduce(amount_sold, sol_received);
            
            // Record the trade
            let record = TradeRecord::new_sell(
                *token_mint,
                trade_type,
                amount_sold,
                sol_received,
                signature,
                pnl,
            );
            self.trade_history.write().push(record);
            
            result = Some(position.clone());
            
            if position.is_closed() {
                info!("📉 Position closed for token: {}", token_mint);
            } else {
                info!(
                    "📉 Reduced position for token: {} (remaining: {})",
                    token_mint,
                    position.amount
                );
            }
        }
        
        // Remove closed positions
        self.positions.retain(|_, p| !p.is_closed());
        
        result
    }
    
    /// Close a position entirely
    pub fn close_position(
        &self,
        token_mint: &Pubkey,
        sol_received: u64,
        trade_type: TradeRecordType,
        signature: String,
    ) -> Option<Position> {
        if let Some(position) = self.get_position(token_mint) {
            self.reduce_position(
                token_mint,
                position.amount,
                sol_received,
                trade_type,
                signature,
            )
        } else {
            None
        }
    }
    
    /// Mark a take profit tier as triggered
    pub fn mark_tp_triggered(&self, token_mint: &Pubkey, tier_index: usize) {
        if let Some(mut position) = self.positions.get_mut(token_mint) {
            position.mark_tp_triggered(tier_index);
        }
    }
    
    /// Get all open positions
    pub fn get_all_positions(&self) -> Vec<Position> {
        self.positions.iter().map(|p| p.clone()).collect()
    }
    
    /// Get open positions count
    pub fn open_positions_count(&self) -> usize {
        self.positions.len()
    }
    
    /// Get trade history
    pub fn get_trade_history(&self) -> Vec<TradeRecord> {
        self.trade_history.read().clone()
    }
    
    /// Get total PnL from trade history
    pub fn get_total_pnl(&self) -> i64 {
        self.trade_history.read()
            .iter()
            .filter_map(|r| r.pnl)
            .sum()
    }
    
    /// Add a pending transaction
    pub fn add_pending_tx(&self, signature: String, token_mint: Pubkey) {
        self.pending_txs.insert(signature, token_mint);
    }
    
    /// Remove a pending transaction
    pub fn remove_pending_tx(&self, signature: &str) -> Option<Pubkey> {
        self.pending_txs.remove(signature).map(|(_, v)| v)
    }
    
    /// Check if a transaction is pending
    pub fn is_tx_pending(&self, signature: &str) -> bool {
        self.pending_txs.contains_key(signature)
    }
    
    /// Export state to JSON (for backup)
    pub fn export_state(&self) -> serde_json::Value {
        let positions: Vec<Position> = self.positions.iter()
            .map(|p| p.clone())
            .collect();
        
        let traded: Vec<String> = self.traded_tokens.iter()
            .map(|e| e.key().to_string())
            .collect();
        
        serde_json::json!({
            "positions": positions,
            "traded_tokens": traded,
            "trade_history": *self.trade_history.read(),
        })
    }
    
    /// Get statistics summary
    pub fn get_stats(&self) -> StateStats {
        let history = self.trade_history.read();
        
        let total_buys = history.iter()
            .filter(|r| matches!(r.trade_type, TradeRecordType::Buy))
            .count();
        
        let total_sells = history.iter()
            .filter(|r| !matches!(r.trade_type, TradeRecordType::Buy))
            .count();
        
        let total_pnl: i64 = history.iter()
            .filter_map(|r| r.pnl)
            .sum();
        
        let winning_trades = history.iter()
            .filter(|r| r.pnl.map(|p| p > 0).unwrap_or(false))
            .count();
        
        let losing_trades = history.iter()
            .filter(|r| r.pnl.map(|p| p < 0).unwrap_or(false))
            .count();
        
        StateStats {
            open_positions: self.positions.len(),
            total_traded_tokens: self.traded_tokens.len(),
            total_buys,
            total_sells,
            total_pnl_lamports: total_pnl,
            winning_trades,
            losing_trades,
        }
    }
}

impl Default for StateManager {
    fn default() -> Self {
        Self::new()
    }
}

/// State statistics
#[derive(Debug, Clone)]
pub struct StateStats {
    pub open_positions: usize,
    pub total_traded_tokens: usize,
    pub total_buys: usize,
    pub total_sells: usize,
    pub total_pnl_lamports: i64,
    pub winning_trades: usize,
    pub losing_trades: usize,
}

impl StateStats {
    /// Get win rate percentage
    pub fn win_rate(&self) -> f64 {
        let total = self.winning_trades + self.losing_trades;
        if total == 0 {
            return 0.0;
        }
        (self.winning_trades as f64 / total as f64) * 100.0
    }
    
    /// Get PnL in SOL
    pub fn total_pnl_sol(&self) -> f64 {
        self.total_pnl_lamports as f64 / 1_000_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_state_manager() {
        let manager = StateManager::new();
        let mint = Pubkey::new_unique();
        
        assert!(!manager.has_traded_token(&mint));
        assert!(manager.can_copy_buy(&mint));
        
        let position = Position::new(
            mint,
            1_000_000,
            100_000_000,
            "target".to_string(),
            "ours".to_string(),
        );
        
        manager.open_position(position);
        
        assert!(manager.has_traded_token(&mint));
        assert!(manager.has_position(&mint));
        assert!(!manager.can_copy_buy(&mint));
    }
    
    #[test]
    fn test_position_closure() {
        let manager = StateManager::new();
        let mint = Pubkey::new_unique();
        
        let position = Position::new(
            mint,
            1_000_000,
            100_000_000,
            "target".to_string(),
            "ours".to_string(),
        );
        
        manager.open_position(position);
        assert_eq!(manager.open_positions_count(), 1);
        
        manager.close_position(
            &mint,
            150_000_000,
            TradeRecordType::SellTakeProfit,
            "sell_sig".to_string(),
        );
        
        assert_eq!(manager.open_positions_count(), 0);
        assert!(manager.has_traded_token(&mint)); // Still marked as traded
    }
}
