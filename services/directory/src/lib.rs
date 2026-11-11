//! The service directory's lookup (ADR-0005 C12, ADR-0011 N9; task S3-4): what a directory does
//! with each `connect` once `origin.directory`'s generated stub has checked the message, the hello
//! and the server end's type and rights (`docs/spec/OIDL.md` §7.5, §7.8).
//!
//! A directory holds, for each interface ID and major its launcher gave it, a handle to the
//! channel of the provider that serves it. `connect` passes the server end it was given to that
//! provider, then replies. For a key it does not hold it closes the end and answers `not_found`;
//! for a provider that cannot take the end now it closes the end and answers `limit`. Nothing is
//! kept past the call, and the directory never waits on a provider: success means the end was
//! passed on, not that the provider has read it.
//!
//! Not decided here: how a launcher fills a directory (on this side, [`Directory::add`]) and how a
//! provider learns of a connection (on this side, [`Providers::pass`]). Both are ABI decisions
//! for ADR-0011 N14 step 4, when a directory first runs in ring 3; until then this runs on the
//! host only, in its tests (`docs/research/2026-11-10-service-directory.md`).
#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )
)]

use origin_directory::oidl_rt::Handle;
use origin_directory::origin_directory::directory::server::{
    ConnectError, ConnectRequest, ConnectResponseArgs, DirectoryServer,
};

#[cfg(test)]
mod tests;

/// What a directory does with the handles it holds. In a directory process these are
/// `channel_write` and `handle_close` on its own table; in the tests, a model of them.
pub trait Providers {
    /// Passes `server` to `provider`, the channel of the provider of `interface_id` at `major`.
    /// On an error the directory still holds `server`.
    fn pass(
        &mut self,
        provider: Handle,
        interface_id: u64,
        major: u16,
        server: Handle,
    ) -> Result<(), Refused>;

    /// Closes `handle`, which the directory holds.
    fn close(&mut self, handle: Handle);
}

/// Why a provider did not take an end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not now: its queue is full, the directory's quota is spent, or the kernel refused the
    /// transfer.
    Full,
    /// Not ever: the provider's end of its channel is closed.
    Gone,
}

/// Why [`Directory::add`] refused an entry. The caller still holds the provider's handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddError {
    /// The table is full.
    Full,
    /// The interface and major have a provider already.
    Taken,
    /// The provider's handle is not a handle.
    Invalid,
}

/// Who wrote a message: what the kernel stamps on it (ADR-0005 B1, C1). A directory answers
/// whoever holds its channel, so this one does not look at it; what a service allows each
/// principal is its own policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caller {
    /// The writer's process ID.
    pub process: u64,
    /// The writer's principal.
    pub principal: u64,
}

/// One provider, and what it provides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    interface_id: u64,
    major: u16,
    provider: Handle,
}

/// A service directory of at most `N` providers, whose handles `P` acts on.
#[derive(Debug)]
pub struct Directory<P, const N: usize> {
    entries: [Option<Entry>; N],
    providers: P,
}

impl<P: Providers, const N: usize> Directory<P, N> {
    /// An empty directory.
    pub const fn new(providers: P) -> Self {
        Self {
            entries: [None; N],
            providers,
        }
    }

    /// Makes `provider` the provider of `interface_id` at `major`: its launcher's choice
    /// (ADR-0011 N9).
    pub fn add(&mut self, interface_id: u64, major: u16, provider: Handle) -> Result<(), AddError> {
        if !provider.is_valid() {
            return Err(AddError::Invalid);
        }
        if self.find(interface_id, major).is_some() {
            return Err(AddError::Taken);
        }
        let free = self
            .entries
            .iter_mut()
            .find(|entry| entry.is_none())
            .ok_or(AddError::Full)?;
        *free = Some(Entry {
            interface_id,
            major,
            provider,
        });
        Ok(())
    }

    /// The provider of `interface_id` at `major`, if this directory holds one.
    pub fn provider(&self, interface_id: u64, major: u16) -> Option<Handle> {
        self.find(interface_id, major)
            .map(|(_, entry)| entry.provider)
    }

    /// How many providers it holds.
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|entry| entry.is_some()).count()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// What acts on its handles.
    pub const fn providers(&self) -> &P {
        &self.providers
    }

    /// Where the provider of `interface_id` at `major` is, and the entry.
    fn find(&self, interface_id: u64, major: u16) -> Option<(usize, Entry)> {
        self.entries
            .iter()
            .enumerate()
            .find_map(|(index, entry)| match entry {
                Some(found) if found.interface_id == interface_id && found.major == major => {
                    Some((index, *found))
                }
                _ => None,
            })
    }
}

impl<P: Providers, const N: usize> DirectoryServer for Directory<P, N> {
    type Caller = Caller;

    fn connect(
        &mut self,
        _caller: &Caller,
        request: ConnectRequest,
    ) -> Result<ConnectResponseArgs, ConnectError> {
        // The generated stub has checked the end: a channel with read, write, wait and transfer.
        let server = request.server.value;
        let Some((index, entry)) = self.find(request.interface_id, request.major) else {
            self.providers.close(server);
            return Err(ConnectError::NotFound);
        };
        match self
            .providers
            .pass(entry.provider, request.interface_id, request.major, server)
        {
            Ok(()) => Ok(ConnectResponseArgs {}),
            Err(Refused::Full) => {
                self.providers.close(server);
                Err(ConnectError::Limit)
            }
            Err(Refused::Gone) => {
                // That provider will never take another: forget it and close its channel.
                self.providers.close(server);
                if let Some(slot) = self.entries.get_mut(index) {
                    *slot = None;
                }
                self.providers.close(entry.provider);
                Err(ConnectError::NotFound)
            }
        }
    }
}
