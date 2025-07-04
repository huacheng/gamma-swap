use std::ops::Deref;

use crate::{
    curve::CurveCalculator,
    error::GammaError,
    states::{
        AmmConfig, ObservationState, PoolState, UserPoolLiquidity, OBSERVATION_SEED, POOL_SEED,
        POOL_VAULT_SEED, USER_POOL_LIQUIDITY_SEED,
    },
    utils::{create_token_account, is_supported_mint, transfer_from_user_to_pool_vault, U128},
    LOCK_LP_AMOUNT,
};
use anchor_lang::{
    accounts::interface_account::InterfaceAccount,
    prelude::*,
    solana_program::{clock, program::invoke, system_instruction},
};
use anchor_spl::{
    associated_token::AssociatedToken,
    token::Token,
    token_2022::spl_token_2022,
    token_interface::{Mint, TokenAccount, TokenInterface},
};

#[derive(Accounts)]
pub struct Initialize<'info> {
    /// Address paying to create the pool. It can be anyone.
    #[account(mut)]
    pub creator: Signer<'info>,

    /// Which amm config the pool belongs to
    pub amm_config: Box<Account<'info, AmmConfig>>,

    /// CHECK: pda authority of the pool to
    /// sign transactions on behalf of the pool
    /// for vault and lp_mint
    #[account(
        seeds = [
            crate::AUTH_SEED.as_bytes(),
        ],
        bump,
    )]
    pub authority: UncheckedAccount<'info>,

    /// Initialize an account to store the pool state
    #[account(
        init,
        seeds = [
            POOL_SEED.as_bytes(),
            amm_config.key().as_ref(),
            token_0_mint.key().as_ref(),
            token_1_mint.key().as_ref(),
        ],
        bump,
        payer = creator,
        space = PoolState::LEN,
    )]
    pub pool_state: AccountLoader<'info, PoolState>,

    #[account(
        init,
        seeds = [
            USER_POOL_LIQUIDITY_SEED.as_bytes(),
            pool_state.key().as_ref(),
            creator.key().as_ref(),
        ],
        bump,
        payer = creator,
        space = UserPoolLiquidity::LEN,
    )]
    pub user_pool_liquidity: Account<'info, UserPoolLiquidity>,

    /// Token_0 mint, the key must smaller than token_1 mint.
    #[account(
        constraint = token_0_mint.key() < token_1_mint.key(),
        mint::token_program = token_0_program,
    )]
    pub token_0_mint: Box<InterfaceAccount<'info, Mint>>,

    /// Token_1 mint, the key must greater than token_0 mint.
    #[account(
        mint::token_program = token_1_program,
    )]
    pub token_1_mint: Box<InterfaceAccount<'info, Mint>>,

    /// creator token 0 account
    #[account(
        mut,
        token::mint = token_0_mint,
        token::authority = creator,
    )]
    pub creator_token_0: Box<InterfaceAccount<'info, TokenAccount>>,

    /// creator token 1 account
    #[account(
        mut,
        token::mint = token_1_mint,
        token::authority = creator,
    )]
    pub creator_token_1: Box<InterfaceAccount<'info, TokenAccount>>,

    /// CHECK: token 0 vault for the pool
    #[account(
        mut,
        seeds = [
            POOL_VAULT_SEED.as_bytes(),
            pool_state.key().as_ref(),
            token_0_mint.key().as_ref(),
        ],
        bump,
    )]
    pub token_0_vault: UncheckedAccount<'info>,

    /// CHECK: token 1 vault for the pool
    #[account(
        mut,
        seeds = [
            POOL_VAULT_SEED.as_bytes(),
            pool_state.key().as_ref(),
            token_1_mint.key().as_ref(),
        ],
        bump,
    )]
    pub token_1_vault: UncheckedAccount<'info>,

    /// create pool fee account
    #[account(
        mut,
        address = crate::create_pool_fee_reveiver::id(),
    )]
    pub create_pool_fee: Box<InterfaceAccount<'info, TokenAccount>>,

    /// an account to store oracle observations
    #[account(
        init,
        seeds = [
            OBSERVATION_SEED.as_bytes(),
            pool_state.key().as_ref(),
        ],
        bump,
        payer = creator,
        space = ObservationState::LEN,
    )]
    pub observation_state: AccountLoader<'info, ObservationState>,

    /// Program to create mint account and mint tokens
    pub token_program: Program<'info, Token>,
    /// Spl token program or token program 2022
    pub token_0_program: Interface<'info, TokenInterface>,
    /// Spl token program or token program 2022
    pub token_1_program: Interface<'info, TokenInterface>,
    /// Program to create an ATA for receiving position NFT
    pub associated_token_program: Program<'info, AssociatedToken>,
    /// To create a new program account
    pub system_program: Program<'info, System>,
    /// Sysvar for program account
    pub rent: Sysvar<'info, Rent>,
}

pub fn initialize(
    ctx: Context<Initialize>,
    init_amount_0: u64,
    init_amount_1: u64,
    mut open_time: u64,
    max_trade_fee_rate: u64,
    volatility_factor: u64,
) -> Result<()> {
    if !(is_supported_mint(&ctx.accounts.token_0_mint)?
        && is_supported_mint(&ctx.accounts.token_1_mint)?)
    {
        return err!(GammaError::NotSupportMint);
    }

    if ctx.accounts.amm_config.disable_create_pool {
        return err!(GammaError::NotApproved);
    }
    let block_timestamp = clock::Clock::get()?.unix_timestamp as u64;
    if open_time <= block_timestamp {
        open_time = block_timestamp + 1;
    }
    if open_time > block_timestamp + ctx.accounts.amm_config.max_open_time {
        return err!(GammaError::InvalidOpenTime);
    }
    // due to stack/heap limitations, we have to create redundant new token vault accounts ourselves
    create_token_account(
        &ctx.accounts.authority.to_account_info(),
        &ctx.accounts.creator.to_account_info(),
        &ctx.accounts.token_0_vault.to_account_info(),
        &ctx.accounts.token_0_mint.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.token_0_program.to_account_info(),
        &[&[
            POOL_VAULT_SEED.as_bytes(),
            ctx.accounts.pool_state.key().as_ref(),
            ctx.accounts.token_0_mint.key().as_ref(),
            &[ctx.bumps.token_0_vault][..],
        ][..]],
    )?;

    create_token_account(
        &ctx.accounts.authority.to_account_info(),
        &ctx.accounts.creator.to_account_info(),
        &ctx.accounts.token_1_vault.to_account_info(),
        &ctx.accounts.token_1_mint.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.token_1_program.to_account_info(),
        &[&[
            POOL_VAULT_SEED.as_bytes(),
            ctx.accounts.pool_state.key().as_ref(),
            ctx.accounts.token_1_mint.key().as_ref(),
            &[ctx.bumps.token_1_vault][..],
        ][..]],
    )?;

    let mut observation_state = ctx.accounts.observation_state.load_init()?;
    observation_state.pool_id = ctx.accounts.pool_state.key();

    let pool_state = &mut ctx.accounts.pool_state.load_init()?;

    // transfer from user to pool vault
    transfer_from_user_to_pool_vault(
        ctx.accounts.creator.to_account_info(),
        ctx.accounts.creator_token_0.to_account_info(),
        ctx.accounts.token_0_vault.to_account_info(),
        ctx.accounts.token_0_mint.to_account_info(),
        ctx.accounts.token_0_program.to_account_info(),
        init_amount_0,
        ctx.accounts.token_0_mint.decimals,
    )?;

    transfer_from_user_to_pool_vault(
        ctx.accounts.creator.to_account_info(),
        ctx.accounts.creator_token_1.to_account_info(),
        ctx.accounts.token_1_vault.to_account_info(),
        ctx.accounts.token_1_mint.to_account_info(),
        ctx.accounts.token_1_program.to_account_info(),
        init_amount_1,
        ctx.accounts.token_1_mint.decimals,
    )?;

    let token_0_vault =
        spl_token_2022::extension::StateWithExtensions::<spl_token_2022::state::Account>::unpack(
            ctx.accounts
                .token_0_vault
                .to_account_info()
                .try_borrow_data()?
                .deref(),
        )?
        .base;
    let token_1_vault =
        spl_token_2022::extension::StateWithExtensions::<spl_token_2022::state::Account>::unpack(
            ctx.accounts
                .token_1_vault
                .to_account_info()
                .try_borrow_data()?
                .deref(),
        )?
        .base;

    CurveCalculator::validate_supply(token_0_vault.amount, token_1_vault.amount)?;

    let liquidity = U128::from(token_0_vault.amount)
        .checked_mul(token_1_vault.amount.into())
        .ok_or(GammaError::MathOverflow)?
        .integer_sqrt()
        .as_u64();
    #[cfg(feature = "enable-log")]
    msg!(
        "liquidity: {}, vault_0_amount: {}, vault_1_amount: {}",
        liquidity,
        token_0_vault.amount,
        token_1_vault.amount,
    );

    // Charge the fee to create a pool
    if ctx.accounts.amm_config.create_pool_fee != 0 {
        invoke(
            &system_instruction::transfer(
                ctx.accounts.creator.key,
                &ctx.accounts.create_pool_fee.key(),
                u64::from(ctx.accounts.amm_config.create_pool_fee),
            ),
            &[
                ctx.accounts.creator.to_account_info(),
                ctx.accounts.create_pool_fee.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
        )?;
        invoke(
            &spl_token::instruction::sync_native(
                ctx.accounts.token_program.key,
                &ctx.accounts.create_pool_fee.key(),
            )?,
            &[
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.create_pool_fee.to_account_info(),
            ],
        )?;
    }

    pool_state.initialize(
        token_0_vault.amount,
        token_1_vault.amount,
        ctx.bumps.authority,
        liquidity,
        open_time,
        max_trade_fee_rate,
        volatility_factor,
        ctx.accounts.creator.key(),
        ctx.accounts.amm_config.key(),
        ctx.accounts.token_0_vault.key(),
        ctx.accounts.token_1_vault.key(),
        &ctx.accounts.token_0_mint,
        &ctx.accounts.token_1_mint,
        ctx.accounts.observation_state.key(),
    )?;

    let user_pool_liquidity = &mut ctx.accounts.user_pool_liquidity;
    let current_time = Clock::get()?.unix_timestamp as u64;
    user_pool_liquidity.initialize(
        ctx.accounts.creator.key(),
        ctx.accounts.pool_state.key(),
        None,
        current_time,
    );
    user_pool_liquidity.token_0_deposited = u128::from(init_amount_0);
    user_pool_liquidity.token_1_deposited = u128::from(init_amount_1);
    user_pool_liquidity.lp_tokens_owned = u128::from(liquidity)
        .checked_sub(LOCK_LP_AMOUNT.into())
        .ok_or(GammaError::MathOverflow)?;

    Ok(())
}
