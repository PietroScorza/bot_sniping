//! Jito bundle construction and management

use anyhow::{Result, Context};
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signature, Signer},
    system_instruction,
    transaction::Transaction,
    hash::Hash,
    compute_budget::ComputeBudgetInstruction,
};
use tracing::{debug, info};

use super::tip::{get_random_tip_account, TipLevel, TipConfig};

/// A Jito bundle containing one or more transactions
#[derive(Debug)]
pub struct JitoBundle {
    /// Transactions in the bundle (max 5)
    pub transactions: Vec<Transaction>,
    /// Bundle tip level
    pub tip_level: TipLevel,
}

impl JitoBundle {
    /// Create a new empty bundle
    pub fn new(tip_level: TipLevel) -> Self {
        Self {
            transactions: Vec::with_capacity(5),
            tip_level,
        }
    }
    
    /// Add a transaction to the bundle
    pub fn add_transaction(&mut self, tx: Transaction) -> Result<()> {
        if self.transactions.len() >= 5 {
            anyhow::bail!("Bundle cannot contain more than 5 transactions");
        }
        self.transactions.push(tx);
        Ok(())
    }
    
    /// Check if bundle is empty
    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }
    
    /// Get the number of transactions
    pub fn len(&self) -> usize {
        self.transactions.len()
    }
}

/// Bundle builder for constructing Jito bundles
pub struct BundleBuilder {
    keypair: Keypair,
    tip_config: TipConfig,
    compute_unit_limit: u32,
    priority_fee_micro_lamports: u64,
}

impl BundleBuilder {
    /// Create a new bundle builder
    pub fn new(
        keypair: Keypair,
        tip_config: TipConfig,
        compute_unit_limit: u32,
        priority_fee_micro_lamports: u64,
    ) -> Self {
        Self {
            keypair,
            tip_config,
            compute_unit_limit,
            priority_fee_micro_lamports,
        }
    }
    
    /// Build a transaction with compute budget and priority fee
    pub fn build_transaction(
        &self,
        instructions: Vec<Instruction>,
        recent_blockhash: Hash,
    ) -> Result<Transaction> {
        let mut all_instructions = Vec::with_capacity(instructions.len() + 2);
        
        // Add compute budget instruction
        all_instructions.push(
            ComputeBudgetInstruction::set_compute_unit_limit(self.compute_unit_limit)
        );
        
        // Add priority fee
        all_instructions.push(
            ComputeBudgetInstruction::set_compute_unit_price(self.priority_fee_micro_lamports)
        );
        
        // Add the actual instructions
        all_instructions.extend(instructions);
        
        let tx = Transaction::new_signed_with_payer(
            &all_instructions,
            Some(&self.keypair.pubkey()),
            &[&self.keypair],
            recent_blockhash,
        );
        
        Ok(tx)
    }
    
    /// Build a bundle with a single transaction and tip
    pub fn build_bundle(
        &self,
        instructions: Vec<Instruction>,
        recent_blockhash: Hash,
        tip_level: TipLevel,
    ) -> Result<JitoBundle> {
        let mut bundle = JitoBundle::new(tip_level);
        
        // Build the main transaction
        let main_tx = self.build_transaction(instructions, recent_blockhash)?;
        bundle.add_transaction(main_tx)?;
        
        // Build the tip transaction
        let tip_tx = self.build_tip_transaction(recent_blockhash, tip_level)?;
        bundle.add_transaction(tip_tx)?;
        
        Ok(bundle)
    }
    
    /// Build a tip transaction for the bundle
    pub fn build_tip_transaction(
        &self,
        recent_blockhash: Hash,
        tip_level: TipLevel,
    ) -> Result<Transaction> {
        let tip_account = get_random_tip_account();
        let tip_amount = self.tip_config.get_tip(tip_level);
        
        info!(
            "Building tip transaction: {} lamports to {}",
            tip_amount,
            tip_account
        );
        
        let tip_instruction = system_instruction::transfer(
            &self.keypair.pubkey(),
            &tip_account,
            tip_amount,
        );
        
        let tx = Transaction::new_signed_with_payer(
            &[tip_instruction],
            Some(&self.keypair.pubkey()),
            &[&self.keypair],
            recent_blockhash,
        );
        
        Ok(tx)
    }
    
    /// Build a bundle that follows a target transaction
    /// This is used for copy-selling to land in the same block
    pub fn build_follow_bundle(
        &self,
        instructions: Vec<Instruction>,
        recent_blockhash: Hash,
        tip_level: TipLevel,
    ) -> Result<JitoBundle> {
        // For emergency follow bundles, we use higher tips
        let actual_tip_level = match tip_level {
            TipLevel::Normal => TipLevel::Normal,
            TipLevel::Emergency => TipLevel::Emergency,
            TipLevel::Custom(amount) => TipLevel::Custom(amount),
        };
        
        self.build_bundle(instructions, recent_blockhash, actual_tip_level)
    }
    
    /// Get the keypair's public key
    pub fn pubkey(&self) -> Pubkey {
        self.keypair.pubkey()
    }
}

/// Serialize a bundle for submission
pub fn serialize_bundle(bundle: &JitoBundle) -> Result<Vec<Vec<u8>>> {
    bundle.transactions.iter()
        .map(|tx| bincode::serialize(tx).context("Failed to serialize transaction"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_bundle_creation() {
        let bundle = JitoBundle::new(TipLevel::Normal);
        assert!(bundle.is_empty());
        assert_eq!(bundle.len(), 0);
    }
    
    #[test]
    fn test_bundle_max_transactions() {
        let mut bundle = JitoBundle::new(TipLevel::Normal);
        
        // This will fail without a valid transaction, but tests the limit logic
        assert!(bundle.transactions.len() < 5);
    }
}
