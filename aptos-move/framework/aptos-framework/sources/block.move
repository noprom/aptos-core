/// This module defines a struct storing the metadata of the block and new block events.
module aptos_framework::block {
    use std::error;
    use std::vector;
    use aptos_std::event::{Self, EventHandle};

    use aptos_framework::timestamp;
    use aptos_framework::system_addresses;
    use aptos_framework::reconfiguration;
    use aptos_framework::stake;

    friend aptos_framework::genesis;

    /// Should be in-sync with BlockResource rust struct in new_block.rs
    struct BlockResource has key {
        /// Height of the current block
        height: u64,
        /// Time period between epochs.
        epoch_interval: u64,
        /// Handle where events with the time of new blocks are emitted
        new_block_events: EventHandle<Self::NewBlockEvent>,
    }

    /// Should be in-sync with NewBlockEvent rust struct in new_block.rs
    struct NewBlockEvent has drop, store {
        epoch: u64,
        round: u64,
        height: u64,
        previous_block_votes_bitvec: vector<u8>,
        proposer: address,
        failed_proposer_indices: vector<u64>,
        /// On-chain time during the block at the given height
        time_microseconds: u64,
    }

    /// The number of new block events does not equal the current block height.
    const ENUM_NEW_BLOCK_EVENTS_DOES_NOT_MATCH_BLOCK_HEIGHT: u64 = 1;
    /// An invalid proposer was provided. Expected the proposer to be the VM or an active validator.
    const EINVALID_PROPOSER: u64 = 2;
    /// Epoch interval cannot be 0.
    const EZERO_EPOCH_INTERVAL: u64 = 3;

    /// This can only be called during Genesis.
    public(friend) fun initialize(account: &signer, epoch_interval_microsecs: u64) {
        system_addresses::assert_aptos_framework(account);
        assert!(epoch_interval_microsecs > 0, error::invalid_argument(EZERO_EPOCH_INTERVAL));

        move_to<BlockResource>(
            account,
            BlockResource {
                height: 0,
                epoch_interval: epoch_interval_microsecs,
                new_block_events: event::new_event_handle<Self::NewBlockEvent>(account),
            }
        );
    }

    /// Update the epoch interval.
    /// Can only be called as part of the Aptos governance proposal process established by the AptosGovernance module.
    public fun update_epoch_interval_microsecs(
        aptos_framework: &signer,
        new_epoch_interval: u64,
    ) acquires BlockResource {
        system_addresses::assert_aptos_framework(aptos_framework);
        assert!(new_epoch_interval > 0, error::invalid_argument(EZERO_EPOCH_INTERVAL));

        let block_metadata = borrow_global_mut<BlockResource>(@aptos_framework);
        block_metadata.epoch_interval = new_epoch_interval;
    }

    /// Return epoch interval in seconds.
    public fun get_epoch_interval_secs(): u64 acquires BlockResource {
        borrow_global<BlockResource>(@aptos_framework).epoch_interval / 1000000
    }

    /// Set the metadata for the current block.
    /// The runtime always runs this before executing the transactions in a block.
    fun block_prologue(
        vm: signer,
        epoch: u64,
        round: u64,
        proposer: address,
        proposer_index_optional: vector<u64>,
        failed_proposer_indices: vector<u64>,
        previous_block_votes_bitvec: vector<u8>,
        timestamp: u64
    ) acquires BlockResource {
        timestamp::assert_operating();
        // Operational constraint: can only be invoked by the VM.
        system_addresses::assert_vm(&vm);

        // Blocks can only be produced by a valid proposer or by the VM itself for Nil blocks (no user txs).
        assert!(
            proposer == @vm_reserved || stake::is_current_epoch_validator(proposer),
            error::permission_denied(EINVALID_PROPOSER),
        );

        let block_metadata_ref = borrow_global_mut<BlockResource>(@aptos_framework);
        block_metadata_ref.height = event::counter(&block_metadata_ref.new_block_events);

        let new_block_event = NewBlockEvent {
            epoch,
            round,
            height: block_metadata_ref.height,
            previous_block_votes_bitvec,
            proposer,
            failed_proposer_indices,
            time_microseconds: timestamp,
        };
        emit_new_block_event(&vm, &mut block_metadata_ref.new_block_events, new_block_event);

        // Performance scores have to be updated before the epoch transition as the transaction that triggers the
        // transition is the last block in the previous epoch.
        stake::update_performance_statistics(proposer_index_optional, failed_proposer_indices);

        if (timestamp - reconfiguration::last_reconfiguration_time() >= block_metadata_ref.epoch_interval) {
            reconfiguration::reconfigure();
        };
    }

    /// Get the current block height
    public fun get_current_block_height(): u64 acquires BlockResource {
        borrow_global<BlockResource>(@aptos_framework).height
    }

    /// Emit the event and update height and global timestamp
    fun emit_new_block_event(vm: &signer, event_handle: &mut EventHandle<NewBlockEvent>, new_block_event: NewBlockEvent) {
        timestamp::update_global_time(vm, new_block_event.proposer, new_block_event.time_microseconds);
        assert!(
            event::counter(event_handle) == new_block_event.height,
            error::invalid_argument(ENUM_NEW_BLOCK_EVENTS_DOES_NOT_MATCH_BLOCK_HEIGHT),
        );
        event::emit_event<NewBlockEvent>(event_handle, new_block_event);
    }

    /// Emit a `NewEpochEvent` event. This function will be invoked by genesis directly to generate the very first
    /// reconfiguration event.
    fun emit_genesis_block_event(vm: signer) acquires BlockResource {
        let block_metadata_ref = borrow_global_mut<BlockResource>(@aptos_framework);
        emit_new_block_event(
            &vm,
            &mut block_metadata_ref.new_block_events,
            NewBlockEvent {
                epoch: 0,
                round: 0,
                height: 0,
                previous_block_votes_bitvec: vector::empty(),
                proposer: @vm_reserved,
                failed_proposer_indices: vector::empty(),
                time_microseconds: 0,
            }
        );
    }

    #[test(aptos_framework = @aptos_framework)]
    public entry fun test_update_epoch_interval(aptos_framework: signer) acquires BlockResource {
        initialize(&aptos_framework, 1);
        assert!(borrow_global<BlockResource>(@aptos_framework).epoch_interval == 1, 0);
        update_epoch_interval_microsecs(&aptos_framework, 2);
        assert!(borrow_global<BlockResource>(@aptos_framework).epoch_interval == 2, 1);
    }

    #[test(aptos_framework = @aptos_framework, account = @0x123)]
    #[expected_failure(abort_code = 0x50003)]
    public entry fun test_update_epoch_interval_unauthorized_should_fail(
        aptos_framework: signer,
        account: signer,
    ) acquires BlockResource {
        initialize(&aptos_framework, 1);
        update_epoch_interval_microsecs(&account, 2);
    }
}
