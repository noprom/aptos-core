// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::accept_type::AcceptType;
use crate::context::Context;
use crate::failpoint::fail_point_poem;
use crate::response::{
    build_not_found, AptosErrorResponse, BadRequestError, BasicErrorWith404, BasicResponse,
    BasicResponseStatus, BasicResultWith404, InternalError,
};
use crate::ApiTags;
use anyhow::Context as AnyhowContext;
use aptos_api_types::{
    AccountData, Address, AptosErrorCode, AsConverter, LedgerInfo, MoveModuleBytecode,
    MoveResource, MoveStructTag, TransactionId, U64,
};
use aptos_types::access_path::AccessPath;
use aptos_types::account_config::AccountResource;
use aptos_types::account_state::AccountState;
use aptos_types::event::EventHandle;
use aptos_types::event::EventKey;
use aptos_types::state_store::state_key::StateKey;
use move_deps::move_core_types::value::MoveValue;
use move_deps::move_core_types::{
    identifier::Identifier,
    language_storage::{ResourceKey, StructTag},
    move_resource::MoveStructType,
};
use poem_openapi::param::Query;
use poem_openapi::{param::Path, OpenApi};
use std::convert::TryInto;
use std::sync::Arc;

pub struct AccountsApi {
    pub context: Arc<Context>,
}

#[OpenApi]
impl AccountsApi {
    /// Get account
    ///
    /// Return high level information about an account such as its sequence number.
    #[oai(
        path = "/accounts/:address",
        method = "get",
        operation_id = "get_account",
        tag = "ApiTags::Accounts"
    )]
    async fn get_account(
        &self,
        accept_type: AcceptType,
        address: Path<Address>,
        ledger_version: Query<Option<U64>>,
    ) -> BasicResultWith404<AccountData> {
        fail_point_poem("endpoint_get_account")?;
        let account = Account::new(self.context.clone(), address.0, ledger_version.0)?;
        account.account(&accept_type)
    }

    /// Get account resources
    ///
    /// This endpoint returns all account resources at a given address at a
    /// specific ledger version (AKA transaction version). If the ledger
    /// version is not specified in the request, the latest ledger version is used.
    ///
    /// The Aptos nodes prune account state history, via a configurable time window (link).
    /// If the requested data has been pruned, the server responds with a 404.
    #[oai(
        path = "/accounts/:address/resources",
        method = "get",
        operation_id = "get_account_resources",
        tag = "ApiTags::Accounts"
    )]
    async fn get_account_resources(
        &self,
        accept_type: AcceptType,
        address: Path<Address>,
        ledger_version: Query<Option<U64>>,
    ) -> BasicResultWith404<Vec<MoveResource>> {
        fail_point_poem("endpoint_get_account_resources")?;
        let account = Account::new(self.context.clone(), address.0, ledger_version.0)?;
        account.resources(&accept_type)
    }

    /// Get account modules
    ///
    /// This endpoint returns all account modules at a given address at a
    /// specific ledger version (AKA transaction version). If the ledger
    /// version is not specified in the request, the latest ledger version is used.
    ///
    /// The Aptos nodes prune account state history, via a configurable time window (link).
    /// If the requested data has been pruned, the server responds with a 404.
    #[oai(
        path = "/accounts/:address/modules",
        method = "get",
        operation_id = "get_account_modules",
        tag = "ApiTags::Accounts"
    )]
    async fn get_account_modules(
        &self,
        accept_type: AcceptType,
        address: Path<Address>,
        ledger_version: Query<Option<U64>>,
    ) -> BasicResultWith404<Vec<MoveModuleBytecode>> {
        fail_point_poem("endpoint_get_account_modules")?;
        let account = Account::new(self.context.clone(), address.0, ledger_version.0)?;
        account.modules(&accept_type)
    }
}

pub struct Account {
    context: Arc<Context>,
    address: Address,
    ledger_version: u64,
    latest_ledger_info: LedgerInfo,
}

impl Account {
    pub fn new(
        context: Arc<Context>,
        address: Address,
        requested_ledger_version: Option<U64>,
    ) -> Result<Self, BasicErrorWith404> {
        let latest_ledger_info = context.get_latest_ledger_info()?;
        let ledger_version: u64 = requested_ledger_version
            .map(|v| v.0)
            .unwrap_or_else(|| latest_ledger_info.version());

        if ledger_version > latest_ledger_info.version() {
            return Err(build_not_found(
                "ledger",
                TransactionId::Version(U64::from(ledger_version)),
                latest_ledger_info.version(),
            ));
        }

        Ok(Self {
            context,
            address,
            ledger_version,
            latest_ledger_info,
        })
    }

    // These functions map directly to endpoint functions.

    pub fn account(self, accept_type: &AcceptType) -> BasicResultWith404<AccountData> {
        let state_key = StateKey::AccessPath(AccessPath::resource_access_path(ResourceKey::new(
            self.address.into(),
            AccountResource::struct_tag(),
        )));

        let state_value = self
            .context
            .get_state_value_poem(&state_key, self.ledger_version)?;

        let state_value = match state_value {
            Some(state_value) => state_value,
            None => return Err(self.resource_not_found(&AccountResource::struct_tag())),
        };

        let account_resource: AccountResource = bcs::from_bytes(&state_value)
            .context("Internal error deserializing response from DB")
            .map_err(BasicErrorWith404::internal)?;
        let account_data: AccountData = account_resource.into();

        BasicResponse::try_from_rust_value((
            account_data,
            &self.latest_ledger_info,
            BasicResponseStatus::Ok,
            accept_type,
        ))
    }

    pub fn resources(self, accept_type: &AcceptType) -> BasicResultWith404<Vec<MoveResource>> {
        let account_state = self.account_state()?;
        let resources = account_state.get_resources();
        let move_resolver = self.context.move_resolver_poem()?;
        let converted_resources = move_resolver
            .as_converter(self.context.db.clone())
            .try_into_resources(resources)
            .context("Failed to build move resource response from data in DB")
            .map_err(BasicErrorWith404::internal)
            .map_err(|e| e.error_code(AptosErrorCode::InvalidBcsInStorageError))?;

        BasicResponse::try_from_rust_value((
            converted_resources,
            &self.latest_ledger_info,
            BasicResponseStatus::Ok,
            accept_type,
        ))
    }

    pub fn modules(self, accept_type: &AcceptType) -> BasicResultWith404<Vec<MoveModuleBytecode>> {
        let mut modules = Vec::new();
        for module in self.account_state()?.into_modules() {
            modules.push(
                MoveModuleBytecode::new(module)
                    .try_parse_abi()
                    .context("Failed to parse move module ABI")
                    .map_err(BasicErrorWith404::internal)
                    .map_err(|e| e.error_code(AptosErrorCode::InvalidBcsInStorageError))?,
            );
        }
        BasicResponse::try_from_rust_value((
            modules,
            &self.latest_ledger_info,
            BasicResponseStatus::Ok,
            accept_type,
        ))
    }

    // Helpers for processing account state.

    fn account_state(&self) -> Result<AccountState, BasicErrorWith404> {
        let state = self
            .context
            .get_account_state(self.address.into(), self.ledger_version)
            .map_err(BasicErrorWith404::internal)
            .map_err(|e| e.error_code(AptosErrorCode::ReadFromStorageError))?
            .ok_or_else(|| self.account_not_found())?;

        Ok(state)
    }

    // Helpers for building errors.

    fn account_not_found(&self) -> BasicErrorWith404 {
        build_not_found(
            "account",
            format!(
                "address({}) and ledger version({})",
                self.address, self.ledger_version
            ),
            self.latest_ledger_info.version(),
        )
    }

    fn resource_not_found(&self, struct_tag: &StructTag) -> BasicErrorWith404 {
        build_not_found(
            "resource",
            format!(
                "address({}), struct tag({}) and ledger version({})",
                self.address, struct_tag, self.ledger_version
            ),
            self.latest_ledger_info.version(),
        )
    }

    fn field_not_found(
        &self,
        struct_tag: &StructTag,
        field_name: &Identifier,
    ) -> BasicErrorWith404 {
        build_not_found(
            "resource",
            format!(
                "address({}), struct tag({}), field name({}) and ledger version({})",
                self.address, struct_tag, field_name, self.ledger_version
            ),
            self.latest_ledger_info.version(),
        )
    }

    // Events specific stuff.

    pub fn find_event_key(
        &self,
        event_handle: MoveStructTag,
        field_name: Identifier,
    ) -> Result<EventKey, BasicErrorWith404> {
        let struct_tag: StructTag = event_handle
            .try_into()
            .context("Given event handle was invalid")
            .map_err(BasicErrorWith404::bad_request)?;

        let resource = self.find_resource(&struct_tag)?;

        let (_id, value) = resource
            .into_iter()
            .find(|(id, _)| id == &field_name)
            .ok_or_else(|| self.field_not_found(&struct_tag, &field_name))?;

        // Serialization should not fail, otherwise it's internal bug
        let event_handle_bytes = bcs::to_bytes(&value)
            .context("Failed to serialize event handle, this is an internal bug")
            .map_err(BasicErrorWith404::internal)?;
        // Deserialization may fail because the bytes are not EventHandle struct type.
        let event_handle: EventHandle = bcs::from_bytes(&event_handle_bytes)
            .context(format!(
                "Deserialization error, field({}) type is not EventHandle struct",
                field_name
            ))
            .map_err(BasicErrorWith404::bad_request)?;
        Ok(*event_handle.key())
    }

    fn find_resource(
        &self,
        struct_tag: &StructTag,
    ) -> Result<Vec<(Identifier, MoveValue)>, BasicErrorWith404> {
        let account_state = self.account_state()?;
        let (typ, data) = account_state
            .get_resources()
            .find(|(tag, _data)| tag == struct_tag)
            .ok_or_else(|| self.resource_not_found(struct_tag))?;
        let move_resolver = self.context.move_resolver_poem()?;
        move_resolver
            .as_converter(self.context.db.clone())
            .move_struct_fields(&typ, data)
            .context("Failed to convert move structs")
            .map_err(BasicErrorWith404::internal)
    }
}
