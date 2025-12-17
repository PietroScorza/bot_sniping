//! Trade executor for building and submitting swap transactions

use anyhow::{Result, Context};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
    program_pack::Pack,
};
use spl_token::state::Account as TokenAccount;
use std::sync::Arc;
use tracing::{info, warn, error, debug};

use crate::config::Config;
use crate::decoder::DexProgram;
use crate::jito::{JitoClient, BundleBuilder, TipLevel, TipConfig};
use crate::state::StateManager;

/// Result of a buy execution
#[derive(Debug)]
pub struct BuyResult {
    pub signature: String,
    pub tokens_received: u64,
    pub sol_spent: u64,
}

/// Result of a sell execution
#[derive(Debug)]
pub struct SellResult {
    pub signature: String,
    pub tokens_sold: u64,
    pub sol_received: u64,
}

/// Trade executor handles the actual swap transaction construction and submission
pub struct TradeExecutor {
    config: Config,
    jito_client: JitoClient,
    rpc_client: RpcClient,
    bundle_builder: BundleBuilder,
    state: Arc<StateManager>,
}

impl TradeExecutor {
    /// Create a new trade executor
    pub fn new(
        config: Config,
        jito_client: JitoClient,
        state: Arc<StateManager>,
    ) -> Result<Self> {
        let rpc_client = RpcClient::new(config.solana_rpc_url.clone());
        
        let tip_config = TipConfig::new(
            config.tip_amount_normal,
            config.tip_amount_emergency,
            config.tip_amount_max,
        );
        
        // Clone the keypair for the bundle builder
        let keypair_bytes = config.keypair.to_bytes();
        let keypair = Keypair::from_bytes(&keypair_bytes)?;
        
        let bundle_builder = BundleBuilder::new(
            keypair,
            tip_config,
            config.compute_unit_limit,
            config.priority_fee_micro_lamports,
        );
        
        Ok(Self {
            config,
            jito_client,
            rpc_client,
            bundle_builder,
            state,
        })
    }
    
    /// Execute a buy order
    pub async fn execute_buy(
        &self,
        token_mint: Pubkey,
        sol_amount: u64,
        dex: DexProgram,
        reference_accounts: &[Pubkey],
        tip_level: TipLevel,
    ) -> Result<BuyResult> {
        info!(
            "Executing buy: {} lamports for token {} on {:?}",
            sol_amount, token_mint, dex
        );
        
        // Get recent blockhash
        let recent_blockhash = self.rpc_client.get_latest_blockhash()
            .context("Failed to get recent blockhash")?;
        
        // Build swap instructions based on DEX
        let instructions = match dex {
            DexProgram::RaydiumAmm => {
                self.build_raydium_buy_instructions(token_mint, sol_amount, reference_accounts)?
            }
            DexProgram::PumpFun => {
                self.build_pumpfun_buy_instructions(token_mint, sol_amount, reference_accounts)?
            }
            DexProgram::Jupiter => {
                self.build_jupiter_buy_instructions(token_mint, sol_amount, reference_accounts)?
            }
            _ => {
                // Fallback to Jupiter aggregator for other DEXes
                self.build_jupiter_buy_instructions(token_mint, sol_amount, reference_accounts)?
            }
        };
        
        // Build and submit bundle
        let bundle = self.bundle_builder.build_bundle(
            instructions,
            recent_blockhash,
            tip_level,
        )?;
        
        let result = self.jito_client.submit_bundle(&bundle).await?;
        
        if !result.success {
            anyhow::bail!("Bundle submission failed: {:?}", result.error);
        }
        
        let bundle_id = result.bundle_id.unwrap_or_default();
        
        // TODO: Implement proper token balance tracking
        // For now, return estimated values
        Ok(BuyResult {
            signature: bundle_id,
            tokens_received: 0, // Will be updated from on-chain data
            sol_spent: sol_amount,
        })
    }
    
    /// Execute a sell order
    /// If close_ata is true and selling 100%, will close the token account to recover rent
    pub async fn execute_sell(
        &self,
        token_mint: Pubkey,
        token_amount: u64,
        dex: DexProgram,
        reference_accounts: &[Pubkey],
        tip_level: TipLevel,
    ) -> Result<SellResult> {
        // Check current balance to determine if this is a full sell
        let current_balance = self.get_token_balance(&token_mint).await.unwrap_or(0);
        let is_full_sell = token_amount >= current_balance && current_balance > 0;
        
        info!(
            "Executing sell: {} tokens of {} on {:?} (tip: {:?}, full_sell: {})",
            token_amount, token_mint, dex, tip_level, is_full_sell
        );
        
        // Get recent blockhash
        let recent_blockhash = self.rpc_client.get_latest_blockhash()
            .context("Failed to get recent blockhash")?;
        
        // Build swap instructions based on DEX
        let mut instructions = match dex {
            DexProgram::RaydiumAmm => {
                self.build_raydium_sell_instructions(token_mint, token_amount, reference_accounts)?
            }
            DexProgram::PumpFun => {
                self.build_pumpfun_sell_instructions(token_mint, token_amount, reference_accounts)?
            }
            DexProgram::Jupiter => {
                self.build_jupiter_sell_instructions(token_mint, token_amount, reference_accounts)?
            }
            _ => {
                self.build_jupiter_sell_instructions(token_mint, token_amount, reference_accounts)?
            }
        };
        
        // If selling 100%, add instruction to close the ATA and recover rent
        if is_full_sell {
            info!("📦 Adding close ATA instruction to recover ~0.002 SOL rent");
            let close_ix = self.build_close_ata_instruction(&token_mint);
            instructions.push(close_ix);
        }
        
        // Build and submit bundle with appropriate tip level
        let bundle = self.bundle_builder.build_bundle(
            instructions,
            recent_blockhash,
            tip_level,
        )?;
        
        let result = self.jito_client.submit_bundle(&bundle).await?;
        
        if !result.success {
            anyhow::bail!("Bundle submission failed: {:?}", result.error);
        }
        
        let bundle_id = result.bundle_id.unwrap_or_default();
        
        if is_full_sell {
            info!("✅ Full sell + ATA closed! Recovered ~0.002 SOL rent");
        }
        
        Ok(SellResult {
            signature: bundle_id,
            tokens_sold: token_amount,
            sol_received: 0, // Will be updated from on-chain data
        })
    }
    
    /// Build Raydium AMM buy instructions
    fn build_raydium_buy_instructions(
        &self,
        token_mint: Pubkey,
        sol_amount: u64,
        reference_accounts: &[Pubkey],
    ) -> Result<Vec<Instruction>> {
        // This is a placeholder - actual implementation requires:
        // 1. Finding the AMM pool for SOL/token pair
        // 2. Getting pool accounts (coin vault, pc vault, etc.)
        // 3. Building the swap instruction with proper account metas
        
        let mut instructions = Vec::new();
        
        // For now, return a placeholder that won't work but shows the structure
        warn!("Raydium buy instruction builder is not fully implemented");
        
        // The actual instruction would look like:
        // instructions.push(raydium_amm_swap_instruction(...));
        
        Ok(instructions)
    }
    
    /// Build Raydium AMM sell instructions
    fn build_raydium_sell_instructions(
        &self,
        token_mint: Pubkey,
        token_amount: u64,
        reference_accounts: &[Pubkey],
    ) -> Result<Vec<Instruction>> {
        warn!("Raydium sell instruction builder is not fully implemented");
        Ok(vec![])
    }
    
    /// Build Pump.fun buy instructions
    fn build_pumpfun_buy_instructions(
        &self,
        token_mint: Pubkey,
        sol_amount: u64,
        reference_accounts: &[Pubkey],
    ) -> Result<Vec<Instruction>> {
        // Pump.fun buy instruction structure:
        // Discriminator: [0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea]
        // Data: amount (u64), max_sol_cost (u64)
        
        // Account layout:
        // 0: global (PDA)
        // 1: fee_recipient
        // 2: mint
        // 3: bonding_curve (PDA)
        // 4: associated_bonding_curve (ATA)
        // 5: associated_user (ATA)
        // 6: user (signer)
        // 7: system_program
        // 8: token_program
        // 9: rent
        // 10: event_authority
        // 11: program
        
        warn!("Pump.fun buy instruction builder is not fully implemented");
        
        // Placeholder - actual implementation needs pool discovery
        let mut data = vec![0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];
        data.extend_from_slice(&0u64.to_le_bytes()); // amount
        data.extend_from_slice(&sol_amount.to_le_bytes()); // max_sol_cost
        
        Ok(vec![])
    }
    
    /// Build Pump.fun sell instructions
    fn build_pumpfun_sell_instructions(
        &self,
        token_mint: Pubkey,
        token_amount: u64,
        reference_accounts: &[Pubkey],
    ) -> Result<Vec<Instruction>> {
        warn!("Pump.fun sell instruction builder is not fully implemented");
        Ok(vec![])
    }
    
    /// Build Jupiter aggregator buy instructions (SOL -> Token)
    fn build_jupiter_buy_instructions(
        &self,
        token_mint: Pubkey,
        sol_amount: u64,
        _reference_accounts: &[Pubkey],
    ) -> Result<Vec<Instruction>> {
        // Jupiter integration typically uses their API to get swap instructions
        // This is a placeholder showing the flow
        
        warn!("Jupiter buy instruction builder requires API integration");
        
        // In production, you would:
        // 1. Call Jupiter Quote API to get the best route
        // 2. Call Jupiter Swap API to get serialized transaction
        // 3. Deserialize and extract instructions
        
        // Example API flow:
        // let quote = jupiter_client.get_quote(WSOL_MINT, token_mint, sol_amount).await?;
        // let swap_tx = jupiter_client.get_swap_transaction(quote).await?;
        // let instructions = deserialize_instructions(swap_tx)?;
        
        Ok(vec![])
    }
    
    /// Build Jupiter aggregator sell instructions (Token -> SOL)
    fn build_jupiter_sell_instructions(
        &self,
        token_mint: Pubkey,
        token_amount: u64,
        _reference_accounts: &[Pubkey],
    ) -> Result<Vec<Instruction>> {
        warn!("Jupiter sell instruction builder requires API integration");
        Ok(vec![])
    }
    
    /// Get the associated token account for a wallet and mint
    fn get_associated_token_address(&self, wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
        spl_associated_token_account::get_associated_token_address(wallet, mint)
    }
    
    /// Check if we have enough SOL balance
    pub async fn check_sol_balance(&self, required_lamports: u64) -> Result<bool> {
        let balance = self.rpc_client.get_balance(&self.bundle_builder.pubkey())
            .context("Failed to get SOL balance")?;
        
        // Add buffer for fees
        let required_with_buffer = required_lamports.saturating_add(100_000_000); // +0.1 SOL buffer
        
        Ok(balance >= required_with_buffer)
    }
    
    /// Get token balance for a specific mint
    pub async fn get_token_balance(&self, mint: &Pubkey) -> Result<u64> {
        let token_account = self.get_associated_token_address(
            &self.bundle_builder.pubkey(),
            mint,
        );
        
        match self.rpc_client.get_token_account_balance(&token_account) {
            Ok(balance) => {
                Ok(balance.amount.parse().unwrap_or(0))
            }
            Err(_) => Ok(0), // Account doesn't exist
        }
    }
    
    /// Build instruction to close an empty token account and recover rent
    pub fn build_close_ata_instruction(&self, token_mint: &Pubkey) -> Instruction {
        let token_account = self.get_associated_token_address(
            &self.bundle_builder.pubkey(),
            token_mint,
        );
        
        spl_token::instruction::close_account(
            &spl_token::id(),
            &token_account,
            &self.bundle_builder.pubkey(), // SOL destination (recover rent here)
            &self.bundle_builder.pubkey(), // Owner
            &[],
        ).expect("Failed to create close account instruction")
    }
    
    /// Close an empty ATA and recover the rent (~0.002 SOL)
    pub async fn close_empty_ata(&self, token_mint: &Pubkey) -> Result<String> {
        // First check if the account exists and is empty
        let balance = self.get_token_balance(token_mint).await?;
        
        if balance > 0 {
            anyhow::bail!("Cannot close ATA with balance > 0. Current balance: {}", balance);
        }
        
        let token_account = self.get_associated_token_address(
            &self.bundle_builder.pubkey(),
            token_mint,
        );
        
        // Check if account exists
        if self.rpc_client.get_account(&token_account).is_err() {
            anyhow::bail!("Token account does not exist");
        }
        
        info!("Closing empty ATA: {} for mint: {}", token_account, token_mint);
        
        let close_ix = self.build_close_ata_instruction(token_mint);
        
        let recent_blockhash = self.rpc_client.get_latest_blockhash()?;
        
        let bundle = self.bundle_builder.build_bundle(
            vec![close_ix],
            recent_blockhash,
            TipLevel::Normal, // Low priority, no rush
        )?;
        
        let result = self.jito_client.submit_bundle(&bundle).await?;
        
        if !result.success {
            anyhow::bail!("Failed to close ATA: {:?}", result.error);
        }
        
        let sig = result.bundle_id.unwrap_or_default();
        info!("✅ ATA closed successfully! Recovered ~0.002 SOL. Signature: {}", sig);
        
        Ok(sig)
    }
    
    /// Find and close all empty ATAs to recover rent
    /// Returns: (number of accounts closed, total SOL recovered in lamports)
    pub async fn close_all_empty_atas(&self) -> Result<(usize, u64)> {
        info!("🔍 Scanning for empty token accounts to close...");
        
        let owner = self.bundle_builder.pubkey();
        
        // Get all token accounts for this wallet
        let token_accounts = self.rpc_client
            .get_token_accounts_by_owner(
                &owner,
                solana_client::rpc_request::TokenAccountsFilter::ProgramId(spl_token::id()),
            )
            .context("Failed to fetch token accounts")?;
        
        let mut empty_accounts: Vec<(Pubkey, Pubkey)> = Vec::new(); // (token_account, mint)
        
        for account in &token_accounts {
            let account_pubkey = Pubkey::from_str(&account.pubkey)
                .context("Invalid account pubkey")?;
            
            // Decode the token account to get mint and balance
            if let Some(data) = &account.account.data.decode() {
                if let Ok(token_account) = TokenAccount::unpack(data) {
                    if token_account.amount == 0 {
                        empty_accounts.push((account_pubkey, token_account.mint));
                    }
                }
            }
        }
        
        if empty_accounts.is_empty() {
            info!("✨ No empty token accounts found");
            return Ok((0, 0));
        }
        
        info!("Found {} empty token accounts to close", empty_accounts.len());
        
        let rent_per_account: u64 = 2_039_280; // ~0.00203928 SOL per ATA
        let mut closed_count = 0;
        let mut total_recovered: u64 = 0;
        
        // Close accounts in batches (max ~5 per transaction to avoid size limits)
        for chunk in empty_accounts.chunks(5) {
            let mut instructions: Vec<Instruction> = Vec::new();
            
            for (token_account, _mint) in chunk {
                let close_ix = spl_token::instruction::close_account(
                    &spl_token::id(),
                    token_account,
                    &owner,
                    &owner,
                    &[],
                )?;
                instructions.push(close_ix);
            }
            
            let recent_blockhash = self.rpc_client.get_latest_blockhash()?;
            
            let bundle = self.bundle_builder.build_bundle(
                instructions,
                recent_blockhash,
                TipLevel::Normal,
            )?;
            
            match self.jito_client.submit_bundle(&bundle).await {
                Ok(result) if result.success => {
                    closed_count += chunk.len();
                    total_recovered += rent_per_account * chunk.len() as u64;
                    info!("✅ Closed {} accounts in batch", chunk.len());
                }
                Ok(result) => {
                    warn!("Batch failed: {:?}", result.error);
                }
                Err(e) => {
                    warn!("Error closing batch: {:?}", e);
                }
            }
            
            // Small delay between batches
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
        
        let sol_recovered = total_recovered as f64 / 1_000_000_000.0;
        info!(
            "🎉 Closed {} empty accounts. Recovered ~{:.4} SOL ({} lamports)",
            closed_count, sol_recovered, total_recovered
        );
        
        Ok((closed_count, total_recovered))
    }
}

use std::str::FromStr;
