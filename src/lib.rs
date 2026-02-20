#![no_std]
use soroban_sdk::{contract, contracttype, contractimpl, Address, Env, Vec, Symbol, token};

// --- DATA STRUCTURES ---

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Circle(u64),
    Member(Address),
    CircleCount,
    // Tracks if a user has paid for a specific circle (CircleID, UserAddress)
    Deposit(u64, Address),
    // Early payout requests
    EarlyPayoutRequest(u64, Address),
    // Tracks Group Reserve balance for penalties
    GroupReserve,
}

#[contracttype]
#[derive(Clone)]
pub struct Member {
    pub address: Address,
    pub has_contributed: bool,
    pub contribution_count: u32,
    pub last_contribution_time: u64,
}

#[contracttype]
#[derive(Clone)]
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

// --- EVENTS MODULE ---

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

// --- CONTRACT TRAIT ---

pub trait SoroSusuTrait {
    // Initialize the contract
    fn init(env: Env, admin: Address);
    
    // Create a new savings circle
    fn create_circle(env: Env, creator: Address, amount: i128, max_members: u32, token: Address, cycle_duration: u64) -> u64;

    // Join an existing circle
    fn join_circle(env: Env, user: Address, circle_id: u64);

    // Make a deposit (Pay your weekly/monthly due)
    fn deposit(env: Env, user: Address, circle_id: u64);

    // Request early payout (emergency)
    fn request_early_payout(env: Env, user: Address, circle_id: u64);

    // Approve early payout (admin only)
    fn approve_early_payout(env: Env, admin: Address, circle_id: u64, user: Address);
}

// --- IMPLEMENTATION ---

#[contract]
pub struct SoroSusu;

#[contractimpl]
impl SoroSusuTrait for SoroSusu {
    fn init(env: Env, admin: Address) {
        if !env.storage().instance().has(&DataKey::CircleCount) {
            env.storage().instance().set(&DataKey::CircleCount, &0u64);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    fn create_circle(env: Env, creator: Address, amount: i128, max_members: u32, token: Address, cycle_duration: u64) -> u64 {
        let mut circle_count: u64 = env.storage().instance().get(&DataKey::CircleCount).unwrap_or(0);
        circle_count += 1;

        let current_time = env.ledger().timestamp();
        let new_circle = CircleInfo {
            id: circle_count,
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

        env.storage().instance().set(&DataKey::Circle(circle_count), &new_circle);
        env.storage().instance().set(&DataKey::CircleCount, &circle_count);

        if !env.storage().instance().has(&DataKey::GroupReserve) {
            env.storage().instance().set(&DataKey::GroupReserve, &0i128);
        }

        // Emit GroupCreated Event
        events::group_created(&env, circle_count, creator, amount);

        circle_count
    }

    fn join_circle(env: Env, user: Address, circle_id: u64) {
        user.require_auth();
        let mut circle: CircleInfo = env.storage().instance().get(&DataKey::Circle(circle_id)).unwrap();

        if circle.member_count >= circle.max_members {
            panic!("Circle is full");
        }

        let member_key = DataKey::Member(user.clone());
        if env.storage().instance().has(&member_key) {
            panic!("User is already a member");
        }

        let new_member = Member {
            address: user.clone(),
            has_contributed: false,
            contribution_count: 0,
            last_contribution_time: 0,
        };
        
        env.storage().instance().set(&member_key, &new_member);
        circle.member_count += 1;
        env.storage().instance().set(&DataKey::Circle(circle_id), &circle);
    }

    fn deposit(env: Env, user: Address, circle_id: u64) {
        user.require_auth();
        let mut circle: CircleInfo = env.storage().instance().get(&DataKey::Circle(circle_id)).unwrap();

        let member_key = DataKey::Member(user.clone());
        let mut member: Member = env.storage().instance().get(&member_key)
            .unwrap_or_else(|| panic!("User is not a member of this circle"));

        let client = token::Client::new(&env, &circle.token);
        let current_time = env.ledger().timestamp();

        let mut total_amount = circle.contribution_amount;
        if current_time > circle.deadline_timestamp {
            let penalty_amount = circle.contribution_amount / 100; // 1% penalty
            let mut reserve_balance: i128 = env.storage().instance().get(&DataKey::GroupReserve).unwrap_or(0);
            reserve_balance += penalty_amount;
            env.storage().instance().set(&DataKey::GroupReserve, &reserve_balance);
            total_amount += penalty_amount;
        }

        // Transfer funds
        client.transfer(
            &user, 
            &env.current_contract_address(), 
            &total_amount
        );

        // Update member info
        member.has_contributed = true;
        member.contribution_count += 1;
        member.last_contribution_time = current_time;
        env.storage().instance().set(&member_key, &member);

        // Update circle deadline
        circle.deadline_timestamp = current_time + circle.cycle_duration;
        env.storage().instance().set(&DataKey::Circle(circle_id), &circle);

        // Backward compatibility
        env.storage().instance().set(&DataKey::Deposit(circle_id, user.clone()), &true);

        // Emit Deposit Event
        events::deposit(&env, user, circle.contribution_amount, current_time);
    }

    fn request_early_payout(env: Env, user: Address, circle_id: u64) {
        user.require_auth();
        if !env.storage().instance().has(&DataKey::EarlyPayoutRequest(circle_id, user.clone())) {
            env.storage().instance().set(&DataKey::EarlyPayoutRequest(circle_id, user), &true);
        }
    }

    fn approve_early_payout(env: Env, admin: Address, circle_id: u64, user: Address) {
        admin.require_auth();
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if admin != stored_admin {
            panic!("Not authorized");
        }

        if env.storage().instance().has(&DataKey::EarlyPayoutRequest(circle_id, user.clone())) {
            let circle: CircleInfo = env.storage().instance().get(&DataKey::Circle(circle_id)).unwrap();
            
            let client = token::Client::new(&env, &circle.token);
            let payout_amount = circle.contribution_amount * (circle.member_count as i128);
            
            client.transfer(
                &env.current_contract_address(),
                &user,
                &payout_amount
            );

            // Emit Payout Event
            events::payout(&env, user.clone(), payout_amount, 1);
            
            env.storage().instance().remove(&DataKey::EarlyPayoutRequest(circle_id, user));
        }
    }
}
