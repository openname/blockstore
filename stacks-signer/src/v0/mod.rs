// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2024 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

/// The signer module for processing events
pub mod signer;

#[cfg(any(test, feature = "testing"))]
/// Test specific functions for the signer module
pub mod tests;

use libsigner::v0::messages::SignerMessage;

use crate::v0::signer::Signer;

/// A v0 spawned signer
pub type SpawnedSigner = crate::SpawnedSigner<Signer, SignerMessage>;
