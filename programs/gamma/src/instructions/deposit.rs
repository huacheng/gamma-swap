use crate::{
    curve::{CurveCalculator, RoundDirection},
    error::GammaError,
    states::{
        LpChangeEvent, PartnerType, PoolState, PoolStatusBitIndex, UserPoolLiquidity,
        USER_POOL_LIQUIDITY_SEED,
    },
    utils::{get_transfer_inverse_fee, transfer_from_user_to_pool_vault},
};
use anchor_lang::prelude::*;
use anchor_spl::{
    token::Token,
    token_interface::{Mint, Token2022, TokenAccount},
};

#[derive(Accounts)]
pub struct Deposit<'info> {
    /// Owner of the liquidity provided
    pub owner: Signer<'info>,

    /// CHECK: pool vault authority
    #[account(
        seeds = [
            crate::AUTH_SEED.as_bytes(),
        ],
        bump,
    )]
    pub authority: UncheckedAccount<'info>,

    /// Pool state the owner is depositing into
    #[account(mut)]
    pub pool_state: AccountLoader<'info, PoolState>,

    #[account(
        mut,
        seeds = [
            USER_POOL_LIQUIDITY_SEED.as_bytes(),
            pool_state.key().as_ref(),
            owner.key().as_ref(),
        ],
        bump,
    )]
    pub user_pool_liquidity: Account<'info, UserPoolLiquidity>,

    /// The payer's token account to deposit token_0
    #[account(
        mut,
        token::mint = token_0_vault.mint,
        token::authority = owner
    )]
    pub token_0_account: Box<InterfaceAccount<'info, TokenAccount>>,

    /// The payer's token account to deposit token_1
    #[account(
        mut,
        token::mint = token_1_vault.mint,
        token::authority = owner
    )]
    pub token_1_account: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Pool vault for token_0 to deposit into
    /// The address that holds pool tokens for token_0
    #[account(
        mut,
        constraint = token_0_vault.key() == pool_state.load()?.token_0_vault
    )]
    pub token_0_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Pool vault for token_1 to deposit into
    /// The address that holds pool tokens for token_1
    #[account(
        mut,
        constraint = token_1_vault.key() == pool_state.load()?.token_1_vault
    )]
    pub token_1_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// token Program
    pub token_program: Program<'info, Token>,

    /// Token program 2022
    pub token_program_2022: Program<'info, Token2022>,

    /// The mint of token_0 vault
    #[account(
        address = token_0_vault.mint
    )]
    pub vault_0_mint: Box<InterfaceAccount<'info, Mint>>,

    /// The mint of token_1 vault
    #[account(
        address = token_1_vault.mint
    )]
    pub vault_1_mint: Box<InterfaceAccount<'info, Mint>>,
}

pub fn deposit(
    ctx: Context<Deposit>,
    lp_token_amount: u64,
    maximum_token_0_amount: u64,
    maximum_token_1_amount: u64,
) -> Result<()> {
    deposit_to_gamma_pool(
        ctx.accounts,
        lp_token_amount,
        maximum_token_0_amount,
        maximum_token_1_amount,
    )
}

pub fn deposit_to_gamma_pool(
    accounts: &mut Deposit,
    lp_token_amount: u64,
    maximum_token_0_amount: u64,
    maximum_token_1_amount: u64,
) -> Result<()> {
    require_gt!(lp_token_amount, 0);
    let pool_id = accounts.pool_state.key();
    let pool_state = &mut accounts.pool_state.load_mut()?;
    if !pool_state.get_status_by_bit(PoolStatusBitIndex::Deposit) {
        return err!(GammaError::NotApproved);
    }
    let (total_token_0_amount, total_token_1_amount) = pool_state.vault_amount_without_fee()?;
    let results = CurveCalculator::lp_tokens_to_trading_tokens(
        u128::from(lp_token_amount),
        u128::from(pool_state.lp_supply),
        u128::from(total_token_0_amount),
        u128::from(total_token_1_amount),
        RoundDirection::Ceiling,
    )
    .ok_or(GammaError::ZeroTradingTokens)?;
    if results.token_0_amount == 0 || results.token_1_amount == 0 {
        return err!(GammaError::ZeroTradingTokens);
    }

    let token_0_amount =
        u64::try_from(results.token_0_amount).map_err(|_| GammaError::MathOverflow)?;
    let (transfer_token_0_amount, transfer_token_0_fee) = {
        let transfer_fee =
            get_transfer_inverse_fee(&accounts.vault_0_mint.to_account_info(), token_0_amount)?;
        (
            token_0_amount
                .checked_add(transfer_fee)
                .ok_or(GammaError::MathOverflow)?,
            transfer_fee,
        )
    };

    let token_1_amount =
        u64::try_from(results.token_1_amount).map_err(|_| GammaError::MathOverflow)?;
    let (transfer_token_1_amount, transfer_token_1_fee) = {
        let transfer_fee =
            get_transfer_inverse_fee(&accounts.vault_1_mint.to_account_info(), token_1_amount)?;
        (
            token_1_amount
                .checked_add(transfer_fee)
                .ok_or(GammaError::MathOverflow)?,
            transfer_fee,
        )
    };
    #[cfg(feature = "enable-log")]
    msg!(
        "results.token_0_amount;{}, results.token_1_amount:{},transfer_token_0_amount:{},transfer_token_0_fee:{},
            transfer_token_1_amount:{},transfer_token_1_fee:{}",
        results.token_0_amount,
        results.token_1_amount,
        transfer_token_0_amount,
        transfer_token_0_fee,
        transfer_token_1_amount,
        transfer_token_1_fee
    );

    emit!(LpChangeEvent {
        pool_id,
        lp_amount_before: pool_state.lp_supply,
        token_0_vault_before: total_token_0_amount,
        token_1_vault_before: total_token_1_amount,
        token_0_amount,
        token_1_amount,
        token_0_transfer_fee: transfer_token_0_fee,
        token_1_transfer_fee: transfer_token_1_fee,
        change_type: 0
    });

    if transfer_token_0_amount > maximum_token_0_amount
        || transfer_token_1_amount > maximum_token_1_amount
    {
        return Err(GammaError::ExceededSlippage.into());
    }

    transfer_from_user_to_pool_vault(
        accounts.owner.to_account_info(),
        accounts.token_0_account.to_account_info(),
        accounts.token_0_vault.to_account_info(),
        accounts.vault_0_mint.to_account_info(),
        if accounts.vault_0_mint.to_account_info().owner == accounts.token_program.key {
            accounts.token_program.to_account_info()
        } else {
            accounts.token_program_2022.to_account_info()
        },
        transfer_token_0_amount,
        accounts.vault_0_mint.decimals,
    )?;

    transfer_from_user_to_pool_vault(
        accounts.owner.to_account_info(),
        accounts.token_1_account.to_account_info(),
        accounts.token_1_vault.to_account_info(),
        accounts.vault_1_mint.to_account_info(),
        if accounts.vault_1_mint.to_account_info().owner == accounts.token_program.key {
            accounts.token_program.to_account_info()
        } else {
            accounts.token_program_2022.to_account_info()
        },
        transfer_token_1_amount,
        accounts.vault_1_mint.decimals,
    )?;

    pool_state.token_0_vault_amount = pool_state
        .token_0_vault_amount
        .checked_add(token_0_amount)
        .ok_or(GammaError::MathOverflow)?;
    pool_state.token_1_vault_amount = pool_state
        .token_1_vault_amount
        .checked_add(token_1_amount)
        .ok_or(GammaError::MathOverflow)?;

    pool_state.lp_supply = pool_state
        .lp_supply
        .checked_add(lp_token_amount)
        .ok_or(GammaError::MathOverflow)?;
    let user_pool_liquidity = &mut accounts.user_pool_liquidity;
    user_pool_liquidity.token_0_deposited = user_pool_liquidity
        .token_0_deposited
        .checked_add(u128::from(token_0_amount))
        .ok_or(GammaError::MathOverflow)?;
    user_pool_liquidity.token_1_deposited = user_pool_liquidity
        .token_1_deposited
        .checked_add(u128::from(token_1_amount))
        .ok_or(GammaError::MathOverflow)?;
    user_pool_liquidity.lp_tokens_owned = user_pool_liquidity
        .lp_tokens_owned
        .checked_add(u128::from(lp_token_amount))
        .ok_or(GammaError::MathOverflow)?;
    pool_state.recent_epoch = Clock::get()?.epoch;

    if let Some(user_pool_liquidity_partner) = user_pool_liquidity.partner {
        let mut pool_state_partners = pool_state.partners;
        let partner: Option<&mut crate::states::PartnerInfo> = pool_state_partners
            .iter_mut()
            .find(|p| PartnerType::new(p.partner_id) == user_pool_liquidity_partner);
        if let Some(partner) = partner {
            partner.lp_token_linked_with_partner = partner
                .lp_token_linked_with_partner
                .checked_add(lp_token_amount)
                .ok_or(GammaError::MathOverflow)?;
        }
        pool_state.partners = pool_state_partners;
    }
    Ok(())
}
