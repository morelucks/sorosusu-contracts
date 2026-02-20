#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, contracterror, Address, Env, Symbol, token};

// --- ERROR CODES ---

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    Unauthorized = 3,
    CircleNotFound = 4,
    CircleFull = 5,
    UserAlreadyInCircle = 6,
    UserNotInCircle = 7,
    InsufficientBalance = 8,
    NoPendingRequest = 9,
}

// --- DATA STRUCTURES ---

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Circle(u64),
    Member(Address),
    CircleCount,
    Deposit(u64, Address),
    EarlyPayoutRequest(u64, Address),
    GroupReserve,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Member {
    pub address: Address,
    pub has_contributed: bool,
    pub contribution_count: u32,
    pub last_contribution_time: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct CircleInfo {
    pub id: u64,
    pub creator: Address,
    pub contribution_amount: i128,
    pub max_members: u32,
    pub member_count: u32,
    pub current_recipient_index: u32,
    pub is_active: bool,
    pub token: Address,
    pub deadline_timestamp: u64,
    pub cycle_duration: u64,
}

// --- CONSTANTS ---
const PENALTY_BPS: i128 = 100; // 1% = 100 Basis Points

// --- EVENTS ---

mod events {
    use soroban_sdk::{Symbol, Address, Env};

    pub fn group_created(env: &Env, id: u64, admin: Address, goal: i128) {
        let topics = (Symbol::new(env, "GroupCreated"), id);
        env.events().publish(topics, (admin, goal));
    }

    pub fn deposit(env: &Env, user: Address, amount: i128, timestamp: u64) {
        let topics = (Symbol::new(env, "Deposit"), user);
        env.events().publish(topics, (amount, timestamp));
    }

    pub fn payout(env: &Env, user: Address, amount: i128, round: u32) {
        let topics = (Symbol::new(env, "Payout"), user);
        env.events().publish(topics, (amount, round));
    }
}

// --- HELPERS ---

fn get_admin(env: &Env) -> Result<Address, Error> {
    env.storage().instance().get(&DataKey::Admin).ok_or(Error::NotInitialized)
}

fn get_circle(env: &Env, id: u64) -> Result<CircleInfo, Error> {
    env.storage().instance().get(&DataKey::Circle(id)).ok_or(Error::CircleNotFound)
}

fn get_member(env: &Env, user: &Address) -> Result<Member, Error> {
    env.storage().instance().get(&DataKey::Member(user.clone())).ok_or(Error::UserNotInCircle)
}

// --- CONTRACT ---

#[contract]
pub struct SoroSusu;

#[contractimpl]
impl SoroSusu {
    /// Initialize the ROSCA contract with an admin.
    pub fn init(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        
        env.storage().instance().set(&DataKey::CircleCount, &0u64);
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::GroupReserve, &0i128);
        Ok(())
    }

    /// Create a new savings circle.
    pub fn create_circle(
        env: Env, 
        creator: Address, 
        amount: i128, 
        max_members: u32, 
        token: Address, 
        cycle_duration: u64
    ) -> Result<u64, Error> {
        creator.require_auth();

        let mut count: u64 = env.storage().instance().get(&DataKey::CircleCount).unwrap_or(0);
        count += 1;

        let current_time = env.ledger().timestamp();
        let circle = CircleInfo {
            id: count,
            creator: creator.clone(),
            contribution_amount: amount,
            max_members,
            member_count: 0,
            current_recipient_index: 0,
            is_active: true,
            token,
            deadline_timestamp: current_time + cycle_duration,
            cycle_duration,
        };

        env.storage().instance().set(&DataKey::Circle(count), &circle);
        env.storage().instance().set(&DataKey::CircleCount, &count);

        events::group_created(&env, count, creator, amount);
        Ok(count)
    }

    /// Join a savings circle.
    pub fn join_circle(env: Env, user: Address, circle_id: u64) -> Result<(), Error> {
        user.require_auth();

        let mut circle = get_circle(&env, circle_id)?;
        if circle.member_count >= circle.max_members {
            return Err(Error::CircleFull);
        }

        let member_key = DataKey::Member(user.clone());
        if env.storage().instance().has(&member_key) {
            return Err(Error::UserAlreadyInCircle);
        }

        let member = Member {
            address: user.clone(),
            has_contributed: false,
            contribution_count: 0,
            last_contribution_time: 0,
        };
        
        env.storage().instance().set(&member_key, &member);
        
        circle.member_count += 1;
        env.storage().instance().set(&DataKey::Circle(circle_id), &circle);
        
        Ok(())
    }

    /// Deposit funds into a circle. Applies 1% penalty if late.
    pub fn deposit(env: Env, user: Address, circle_id: u64) -> Result<(), Error> {
        user.require_auth();

        let mut circle = get_circle(&env, circle_id)?;
        let mut member = get_member(&env, &user)?;

        let client = token::Client::new(&env, &circle.token);
        let current_time = env.ledger().timestamp();

        let mut final_amount = circle.contribution_amount;
        
        // Late penalty logic (1%)
        if current_time > circle.deadline_timestamp {
            let penalty = circle.contribution_amount / (10000 / PENALTY_BPS);
            let mut reserve: i128 = env.storage().instance().get(&DataKey::GroupReserve).unwrap_or(0);
            reserve += penalty;
            env.storage().instance().set(&DataKey::GroupReserve, &reserve);
            final_amount += penalty;
        }

        client.transfer(&user, &env.current_contract_address(), &final_amount);

        // Update member state
        member.has_contributed = true;
        member.contribution_count += 1;
        member.last_contribution_time = current_time;
        env.storage().instance().set(&DataKey::Member(user.clone()), &member);

        // Update circle deadline for next cycle
        circle.deadline_timestamp = current_time + circle.cycle_duration;
        env.storage().instance().set(&DataKey::Circle(circle_id), &circle);

        // Track deposit for payout logic
        env.storage().instance().set(&DataKey::Deposit(circle_id, user.clone()), &true);

        events::deposit(&env, user, circle.contribution_amount, current_time);
        Ok(())
    }

    /// Request an emergency early payout.
    pub fn request_early_payout(env: Env, user: Address, circle_id: u64) -> Result<(), Error> {
        user.require_auth();
        let _ = get_circle(&env, circle_id)?;
        let _ = get_member(&env, &user)?;

        env.storage().instance().set(&DataKey::EarlyPayoutRequest(circle_id, user), &true);
        Ok(())
    }

    /// Approve an early payout (Admin Only).
    pub fn approve_early_payout(env: Env, admin: Address, circle_id: u64, user: Address) -> Result<(), Error> {
        admin.require_auth();
        
        let stored_admin = get_admin(&env)?;
        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        let request_key = DataKey::EarlyPayoutRequest(circle_id, user.clone());
        if !env.storage().instance().has(&request_key) {
            return Err(Error::NoPendingRequest);
        }

        let circle = get_circle(&env, circle_id)?;
        let client = token::Client::new(&env, &circle.token);
        
        // Calculate pot: contribution * members
        let payout_amount = circle.contribution_amount * (circle.member_count as i128);
        
        client.transfer(&env.current_contract_address(), &user, &payout_amount);

        events::payout(&env, user.clone(), payout_amount, 1);
        
        env.storage().instance().remove(&request_key);
        Ok(())
    }

    /// Administrative: Get the group reserve balance.
    pub fn get_reserve(env: Env) -> i128 {
        env.storage().instance().get(&DataKey::GroupReserve).unwrap_or(0)
    }
}
