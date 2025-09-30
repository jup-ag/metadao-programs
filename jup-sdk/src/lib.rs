use anchor_lang::prelude::*;

use anyhow::{anyhow, bail, Context, Result};
use jupiter_amm_interface::{
    AccountMap, Amm, AmmContext, AmmProgramIdToLabel, KeyedAccount, Quote, Swap,
    SwapAndAccountMetas, SwapMode, SwapParams,
};

pub mod futarchy_amm;

pub use futarchy_amm::{FutarchyAmm, MAX_BPS, TAKER_FEE_BPS};
use rust_decimal::Decimal;

use crate::futarchy_amm::{FutarchyAmmSwap, SwapType};

declare_id!("FUTARELBfJfQ8RDGhg1wdhddq1odMAJUePHFuBYfUxKq");

pub const SPL_TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const FUTARCHY_EVENT_AUTHORITY_KEY: Pubkey =
    Pubkey::from_str_const("DGEympSS4qLvdr9r3uGHTfACdN8snShk4iGdJtZPxuBC");

impl AmmProgramIdToLabel for FutarchyAmmClient {
    const PROGRAM_ID_TO_LABELS: &[(Pubkey, jupiter_amm_interface::AmmLabel)] =
        &[(ID, "MetaDAO AMM")];
}

#[derive(Debug)]
pub enum FutarchyAmmError {
    MathOverflow,
    InvalidReserves,
    AmmInvariantViolated,
    InvalidQuoteParams,
    ExactOutNotSupported,
    InvalidAmmData,
}

impl std::fmt::Display for FutarchyAmmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Clone)]
pub struct FutarchyAmmClient {
    pub dao_address: Pubkey,
    pub state: FutarchyAmm,
}

impl Amm for FutarchyAmmClient {
    fn label(&self) -> String {
        "MetaDAO AMM".to_string()
    }

    fn program_id(&self) -> Pubkey {
        ID
    }

    fn key(&self) -> Pubkey {
        self.dao_address
    }

    fn get_reserve_mints(&self) -> Vec<Pubkey> {
        vec![self.state.base_mint, self.state.quote_mint]
    }

    fn get_accounts_to_update(&self) -> Vec<Pubkey> {
        vec![self.dao_address]
    }

    fn update(&mut self, account_map: &AccountMap) -> Result<()> {
        let dao_account = account_map.get(&self.dao_address).with_context(|| {
            format!(
                "DAO account not found for dao address: {}",
                self.dao_address
            )
        })?;

        if dao_account.data.len() < 8 + FutarchyAmm::INIT_SPACE {
            bail!(FutarchyAmmError::InvalidAmmData);
        }

        // we don't do Dao deserialization in case it changes, just deserialize the amm
        let amm_data =
            FutarchyAmm::deserialize(&mut &dao_account.data[8..8 + FutarchyAmm::INIT_SPACE])?;

        self.state = amm_data;

        Ok(())
    }

    fn get_accounts_len(&self) -> usize {
        9
    }

    fn from_keyed_account(keyed_account: &KeyedAccount, _amm_context: &AmmContext) -> Result<Self>
    where
        Self: Sized,
    {
        if keyed_account.account.data.len() < 8 + FutarchyAmm::INIT_SPACE {
            bail!(FutarchyAmmError::InvalidAmmData);
        }

        let amm_data = FutarchyAmm::deserialize(
            &mut &keyed_account.account.data[8..8 + FutarchyAmm::INIT_SPACE],
        )?;

        Ok(Self {
            dao_address: keyed_account.key,
            state: amm_data,
        })
    }

    fn get_swap_and_account_metas(&self, swap_params: &SwapParams) -> Result<SwapAndAccountMetas> {
        let SwapParams {
            source_mint,
            destination_token_account,
            source_token_account,
            token_transfer_authority,
            ..
        } = swap_params;

        let (user_base_account, user_quote_account) = if *source_mint == self.state.base_mint {
            (*source_token_account, *destination_token_account)
        } else {
            (*destination_token_account, *source_token_account)
        };

        Ok(SwapAndAccountMetas {
            swap: Swap::TokenSwap,
            account_metas: FutarchyAmmSwap {
                dao: self.dao_address,
                trader: *token_transfer_authority,
                user_base_account,
                user_quote_account,
                amm_base_vault: self.state.amm_base_vault,
                amm_quote_vault: self.state.amm_quote_vault,
                token_program: SPL_TOKEN_PROGRAM_ID,
                futarchy_program: ID,
                futarchy_event_authority: FUTARCHY_EVENT_AUTHORITY_KEY,
            }
            .into(),
        })
    }

    fn clone_amm(&self) -> Box<dyn Amm + Send + Sync> {
        Box::new(self.clone())
    }

    fn quote(
        &self,
        quote_params: &jupiter_amm_interface::QuoteParams,
    ) -> Result<jupiter_amm_interface::Quote> {
        let swap_type = if quote_params.input_mint == self.state.quote_mint
            && quote_params.output_mint == self.state.base_mint
        {
            SwapType::Buy
        } else if quote_params.input_mint == self.state.base_mint
            && quote_params.output_mint == self.state.quote_mint
        {
            SwapType::Sell
        } else {
            bail!(FutarchyAmmError::InvalidQuoteParams);
        };

        if quote_params.swap_mode == SwapMode::ExactOut {
            bail!(FutarchyAmmError::ExactOutNotSupported);
        }

        let out_amount = self.state.state.swap(quote_params.amount, swap_type)?;

        let fee_pct = Decimal::new(TAKER_FEE_BPS as i64, 2);

        // this isn't exact because of compounding, but should be close enough
        let fee_amount = (quote_params.amount as u128)
            .checked_mul(TAKER_FEE_BPS as u128)
            .ok_or_else(|| anyhow!(FutarchyAmmError::MathOverflow))?
            .checked_div(MAX_BPS as u128)
            .ok_or_else(|| anyhow!(FutarchyAmmError::MathOverflow))?
            as u64;

        Ok(Quote {
            in_amount: quote_params.amount,
            out_amount,
            fee_amount,
            fee_mint: quote_params.input_mint,
            fee_pct,
        })
    }
}
