//! DEX-specific instruction decoders

use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

/// Known DEX programs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DexProgram {
    RaydiumAmm,
    RaydiumClmm,
    Jupiter,
    PumpFun,
    OrcaWhirlpool,
    Unknown,
}

impl DexProgram {
    /// Identify DEX from program ID
    pub fn from_program_id(program_id: &Pubkey) -> Self {
        let id_str = program_id.to_string();
        
        match id_str.as_str() {
            "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8" => DexProgram::RaydiumAmm,
            "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK" => DexProgram::RaydiumClmm,
            "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4" => DexProgram::Jupiter,
            "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P" => DexProgram::PumpFun,
            "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc" => DexProgram::OrcaWhirlpool,
            _ => DexProgram::Unknown,
        }
    }
    
    /// Get the program ID for this DEX
    pub fn program_id(&self) -> Option<Pubkey> {
        match self {
            DexProgram::RaydiumAmm => Pubkey::from_str("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8").ok(),
            DexProgram::RaydiumClmm => Pubkey::from_str("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK").ok(),
            DexProgram::Jupiter => Pubkey::from_str("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4").ok(),
            DexProgram::PumpFun => Pubkey::from_str("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P").ok(),
            DexProgram::OrcaWhirlpool => Pubkey::from_str("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc").ok(),
            DexProgram::Unknown => None,
        }
    }
}

/// Raydium AMM instruction discriminators
pub mod raydium {
    /// Swap instruction discriminator (first 8 bytes)
    pub const SWAP_BASE_IN: u8 = 9;
    pub const SWAP_BASE_OUT: u8 = 11;
    
    /// Decode Raydium swap instruction
    pub fn decode_swap(data: &[u8]) -> Option<RaydiumSwap> {
        if data.is_empty() {
            return None;
        }
        
        let discriminator = data[0];
        
        match discriminator {
            SWAP_BASE_IN => {
                if data.len() < 17 {
                    return None;
                }
                let amount_in = u64::from_le_bytes(data[1..9].try_into().ok()?);
                let min_amount_out = u64::from_le_bytes(data[9..17].try_into().ok()?);
                
                Some(RaydiumSwap {
                    is_base_in: true,
                    amount_in,
                    amount_out: min_amount_out,
                })
            }
            SWAP_BASE_OUT => {
                if data.len() < 17 {
                    return None;
                }
                let max_amount_in = u64::from_le_bytes(data[1..9].try_into().ok()?);
                let amount_out = u64::from_le_bytes(data[9..17].try_into().ok()?);
                
                Some(RaydiumSwap {
                    is_base_in: false,
                    amount_in: max_amount_in,
                    amount_out,
                })
            }
            _ => None,
        }
    }
    
    #[derive(Debug, Clone)]
    pub struct RaydiumSwap {
        pub is_base_in: bool,
        pub amount_in: u64,
        pub amount_out: u64,
    }
}

/// Jupiter instruction decoders
pub mod jupiter {
    use super::*;
    
    /// Jupiter route discriminator (Anchor)
    pub const ROUTE_DISCRIMINATOR: [u8; 8] = [0xe5, 0x17, 0xcb, 0x97, 0x7a, 0xe3, 0xad, 0x2a];
    pub const SHARED_ACCOUNTS_ROUTE: [u8; 8] = [0xc1, 0x20, 0x9b, 0x33, 0x41, 0xd6, 0x9c, 0x81];
    
    /// Decode Jupiter swap instruction
    pub fn decode_swap(data: &[u8]) -> Option<JupiterSwap> {
        if data.len() < 8 {
            return None;
        }
        
        let discriminator: [u8; 8] = data[0..8].try_into().ok()?;
        
        if discriminator == ROUTE_DISCRIMINATOR || discriminator == SHARED_ACCOUNTS_ROUTE {
            // Parse route parameters
            // The structure varies, but we can extract basic info
            Some(JupiterSwap {
                is_route: discriminator == ROUTE_DISCRIMINATOR,
            })
        } else {
            None
        }
    }
    
    #[derive(Debug, Clone)]
    pub struct JupiterSwap {
        pub is_route: bool,
    }
}

/// Pump.fun instruction decoders
pub mod pumpfun {
    /// Buy discriminator for Pump.fun
    pub const BUY_DISCRIMINATOR: [u8; 8] = [0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];
    /// Sell discriminator for Pump.fun
    pub const SELL_DISCRIMINATOR: [u8; 8] = [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];
    
    /// Decode Pump.fun instruction
    pub fn decode_instruction(data: &[u8]) -> Option<PumpFunAction> {
        if data.len() < 8 {
            return None;
        }
        
        let discriminator: [u8; 8] = data[0..8].try_into().ok()?;
        
        if discriminator == BUY_DISCRIMINATOR {
            // Buy: amount (u64), max_sol_cost (u64)
            if data.len() >= 24 {
                let amount = u64::from_le_bytes(data[8..16].try_into().ok()?);
                let max_sol_cost = u64::from_le_bytes(data[16..24].try_into().ok()?);
                return Some(PumpFunAction::Buy { amount, max_sol_cost });
            }
        } else if discriminator == SELL_DISCRIMINATOR {
            // Sell: amount (u64), min_sol_output (u64)
            if data.len() >= 24 {
                let amount = u64::from_le_bytes(data[8..16].try_into().ok()?);
                let min_sol_output = u64::from_le_bytes(data[16..24].try_into().ok()?);
                return Some(PumpFunAction::Sell { amount, min_sol_output });
            }
        }
        
        None
    }
    
    #[derive(Debug, Clone)]
    pub enum PumpFunAction {
        Buy { amount: u64, max_sol_cost: u64 },
        Sell { amount: u64, min_sol_output: u64 },
    }
}

/// Orca Whirlpool instruction decoders
pub mod orca {
    /// Swap discriminator for Orca Whirlpool
    pub const SWAP_DISCRIMINATOR: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];
    
    /// Decode Orca swap instruction
    pub fn decode_swap(data: &[u8]) -> Option<OrcaSwap> {
        if data.len() < 8 {
            return None;
        }
        
        let discriminator: [u8; 8] = data[0..8].try_into().ok()?;
        
        if discriminator == SWAP_DISCRIMINATOR && data.len() >= 26 {
            let amount = u64::from_le_bytes(data[8..16].try_into().ok()?);
            let other_amount_threshold = u64::from_le_bytes(data[16..24].try_into().ok()?);
            let a_to_b = data[24] != 0;
            let amount_specified_is_input = data[25] != 0;
            
            return Some(OrcaSwap {
                amount,
                other_amount_threshold,
                a_to_b,
                amount_specified_is_input,
            });
        }
        
        None
    }
    
    #[derive(Debug, Clone)]
    pub struct OrcaSwap {
        pub amount: u64,
        pub other_amount_threshold: u64,
        pub a_to_b: bool,
        pub amount_specified_is_input: bool,
    }
}
