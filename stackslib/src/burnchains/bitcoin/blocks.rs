// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020 Stacks Open Internet Foundation
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

use std::ops::Deref;

use stacks_common::deps_common::bitcoin::blockdata::block::{Block, LoneBlockHeader};
use stacks_common::deps_common::bitcoin::blockdata::opcodes::All as btc_opcodes;
use stacks_common::deps_common::bitcoin::blockdata::script::{Instruction, Script};
use stacks_common::deps_common::bitcoin::blockdata::transaction::Transaction;
use stacks_common::deps_common::bitcoin::network::message as btc_message;
use stacks_common::deps_common::bitcoin::network::serialize::BitcoinHash;
use stacks_common::deps_common::bitcoin::util::hash::bitcoin_merkle_root;
use stacks_common::types::chainstate::BurnchainHeaderHash;
use stacks_common::util::hash::to_hex;
use stacks_common::util::log;

use crate::burnchains::bitcoin::address::BitcoinAddress;
use crate::burnchains::bitcoin::indexer::BitcoinIndexer;
use crate::burnchains::bitcoin::keys::BitcoinPublicKey;
use crate::burnchains::bitcoin::messages::BitcoinMessageHandler;
use crate::burnchains::bitcoin::{
    bits, BitcoinBlock, BitcoinInputType, BitcoinNetworkType, BitcoinTransaction, BitcoinTxInput,
    BitcoinTxOutput, Error as btc_error, PeerMessage,
};
use crate::burnchains::indexer::{
    BurnBlockIPC, BurnHeaderIPC, BurnchainBlockDownloader, BurnchainBlockParser,
};
use crate::burnchains::{
    BurnchainBlock, BurnchainTransaction, Error as burnchain_error, MagicBytes, Txid,
    MAGIC_BYTES_LENGTH,
};
use crate::core::StacksEpochId;
use crate::deps;

#[derive(Debug, Clone, PartialEq)]
pub struct BitcoinHeaderIPC {
    pub block_header: LoneBlockHeader,
    pub block_height: u64,
}

impl BurnHeaderIPC for BitcoinHeaderIPC {
    type H = LoneBlockHeader;

    fn header(&self) -> LoneBlockHeader {
        self.block_header.clone()
    }

    fn height(&self) -> u64 {
        self.block_height
    }

    fn header_hash(&self) -> [u8; 32] {
        self.block_header.header.bitcoin_hash().0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BitcoinBlockIPC {
    pub header_data: BitcoinHeaderIPC,
    pub block_message: PeerMessage,
}

impl BurnBlockIPC for BitcoinBlockIPC {
    type H = BitcoinHeaderIPC;
    type B = PeerMessage;

    fn header(&self) -> BitcoinHeaderIPC {
        self.header_data.clone()
    }

    fn height(&self) -> u64 {
        self.header_data.height()
    }

    fn block(&self) -> PeerMessage {
        self.block_message.clone()
    }
}

pub struct BitcoinBlockDownloader {
    cur_request: Option<BitcoinHeaderIPC>,
    cur_block: Option<BitcoinBlockIPC>,
    indexer: Option<BitcoinIndexer>,
}

pub struct BitcoinBlockParser {
    network_id: BitcoinNetworkType,
    magic_bytes: MagicBytes,
}

impl BitcoinBlockDownloader {
    pub fn new(indexer: BitcoinIndexer) -> BitcoinBlockDownloader {
        BitcoinBlockDownloader {
            cur_request: None,
            cur_block: None,
            indexer: Some(indexer),
        }
    }

    pub fn run(&mut self, header: &BitcoinHeaderIPC) -> Result<BitcoinBlockIPC, btc_error> {
        self.cur_request = Some((*header).clone());

        // should always work, since at most one thread can call this method at once
        // due to &mut self.
        let mut indexer = self.indexer.take().unwrap();

        indexer.peer_communicate(self, false)?;

        self.indexer = Some(indexer);

        assert!(
            self.cur_block.is_some(),
            "BUG: should have received block on 'ok' condition"
        );
        let ipc_block = self.cur_block.take().unwrap();
        Ok(ipc_block)
    }
}

impl BurnchainBlockDownloader for BitcoinBlockDownloader {
    type H = BitcoinHeaderIPC;
    type B = BitcoinBlockIPC;

    fn download(&mut self, header: &BitcoinHeaderIPC) -> Result<BitcoinBlockIPC, burnchain_error> {
        self.run(header).map_err(|e| match e {
            btc_error::TimedOut => burnchain_error::TrySyncAgain,
            x => burnchain_error::DownloadError(x),
        })
    }
}

impl BitcoinMessageHandler for BitcoinBlockDownloader {
    /// Trait message handler
    /// initiate the conversation with the bitcoin peer
    fn begin_session(&mut self, indexer: &mut BitcoinIndexer) -> Result<bool, btc_error> {
        match self.cur_request {
            None => panic!("No block header set"),
            Some(ref ipc_header) => {
                let block_hash = ipc_header.block_header.header.bitcoin_hash().clone();
                indexer.send_getdata(&[block_hash]).map(|_r| true)
            }
        }
    }

    /// Trait message handler
    /// Wait for a block to arrive that matches self.cur_request
    fn handle_message(
        &mut self,
        indexer: &mut BitcoinIndexer,
        msg: PeerMessage,
    ) -> Result<bool, btc_error> {
        // send to our consumer thread for parsing
        if self.cur_block.is_some() {
            debug!("Already have a block");
            return Ok(false);
        }

        if self.cur_request.is_none() {
            // weren't expecting this
            warn!("Unexpected block message");
            return Err(btc_error::InvalidReply);
        }

        let ipc_header = self.cur_request.as_ref().unwrap();

        let height;
        let header;
        let block_hash;

        match msg {
            btc_message::NetworkMessage::Block(ref block) => {
                // make sure this block matches
                if !BitcoinBlockParser::check_block(block, &ipc_header.block_header) {
                    debug!(
                        "Requested block {}, got block {}",
                        &to_hex(ipc_header.block_header.header.bitcoin_hash().as_bytes()),
                        &to_hex(block.bitcoin_hash().as_bytes())
                    );

                    // try again
                    indexer.send_getdata(&[ipc_header.block_header.header.bitcoin_hash()])?;
                    return Ok(true);
                }

                // clear timeout
                indexer.runtime.last_getdata_send_time = 0;

                // got valid data!
                height = ipc_header.block_height;
                header = self.cur_request.clone().unwrap();
                block_hash = ipc_header.block_header.header.bitcoin_hash();
            }
            _ => {
                return Err(btc_error::UnhandledMessage(msg));
            }
        }

        debug!(
            "Got block {}: {}",
            height,
            &to_hex(BurnchainHeaderHash::from_bitcoin_hash(&block_hash).as_bytes())
        );

        // store response. we're done.
        let ipc_block = BitcoinBlockIPC {
            header_data: header,
            block_message: msg,
        };

        self.cur_block = Some(ipc_block);
        Ok(false)
    }
}

impl BitcoinBlockParser {
    /// New block parser
    pub fn new(network_id: BitcoinNetworkType, magic_bytes: MagicBytes) -> BitcoinBlockParser {
        BitcoinBlockParser {
            network_id,
            magic_bytes: magic_bytes.clone(),
        }
    }

    /// Allow raw inputs?
    fn allow_raw_inputs(epoch_id: StacksEpochId) -> bool {
        epoch_id >= StacksEpochId::Epoch21
    }

    /// Allow segwit outputs?
    fn allow_segwit_outputs(epoch_id: StacksEpochId) -> bool {
        epoch_id >= StacksEpochId::Epoch21
    }

    /// Verify that a block matches a header
    pub fn check_block(block: &Block, header: &LoneBlockHeader) -> bool {
        if header.header.bitcoin_hash() != block.bitcoin_hash() {
            return false;
        }

        // block transactions must match header merkle root
        let tx_merkle_root =
            bitcoin_merkle_root(block.txdata.iter().map(|ref tx| tx.txid()).collect());

        if block.header.merkle_root != tx_merkle_root {
            return false;
        }

        true
    }

    /// Parse the data output to get a byte payload
    fn parse_data(&self, data_output: &Script) -> Option<(u8, Vec<u8>)> {
        if !data_output.is_op_return() {
            test_debug!("Data output is not an OP_RETURN");
            return None;
        }

        if data_output.len() <= self.magic_bytes.len() {
            test_debug!("Data output is too short to carry an operation");
            return None;
        }

        let script_pieces = bits::parse_script(&data_output);
        if script_pieces.len() != 2 {
            // not OP_RETURN <data>
            test_debug!("Data output does not encode a valid OP_RETURN");
            return None;
        }

        match (&script_pieces[0], &script_pieces[1]) {
            (Instruction::Op(ref opcode), Instruction::PushBytes(ref data)) => {
                if *opcode != btc_opcodes::OP_RETURN {
                    test_debug!("Data output does not use a standard OP_RETURN");
                    return None;
                }
                if !data.starts_with(self.magic_bytes.as_bytes()) {
                    test_debug!("Data output does not start with magic bytes");
                    return None;
                }

                let opcode = data[MAGIC_BYTES_LENGTH];
                Some((opcode, data[MAGIC_BYTES_LENGTH + 1..data.len()].to_vec()))
            }
            (_, _) => {
                test_debug!("Data output is not OP_RETURN <data>");
                None
            }
        }
    }

    /// Is this an acceptable transaction?  It must have
    /// * an OP_RETURN output at output 0
    /// * only p2pkh or p2sh outputs for outputs 1...n
    fn maybe_burnchain_tx(&self, tx: &Transaction, epoch_id: StacksEpochId) -> bool {
        if self.parse_data(&tx.output[0].script_pubkey).is_none() {
            test_debug!("Tx {:?} has no valid OP_RETURN", tx.txid());
            return false;
        }

        for i in 1..tx.output.len() {
            if epoch_id < StacksEpochId::Epoch21 {
                // only support legacy addresses pre-2.1
                if !tx.output[i].script_pubkey.is_p2pkh() && !tx.output[i].script_pubkey.is_p2sh() {
                    // unrecognized output type
                    test_debug!(
                        "Tx {:?} has unrecognized output type in output {}",
                        tx.txid(),
                        i
                    );
                    return false;
                }
            } else {
                // in 2.1 and later, support it if the output decodes
                if BitcoinAddress::from_scriptpubkey(
                    BitcoinNetworkType::Mainnet,
                    &tx.output[i].script_pubkey.to_bytes(),
                )
                .is_none()
                {
                    test_debug!(
                        "Tx {:?} has unrecognized output type in output {}",
                        tx.txid(),
                        i
                    );
                    return false;
                }
            }
        }

        return true;
    }

    /// Parse a transaction's inputs into burnchain tx inputs.
    /// Succeeds only if we can parse each input into a structured input.
    /// (this is the behavior of Stacks 2.05 and earlier)
    fn parse_inputs_structured(tx: &Transaction) -> Option<Vec<BitcoinTxInput>> {
        let mut ret = vec![];
        for inp in &tx.input {
            match BitcoinTxInput::from_bitcoin_txin_structured(&inp) {
                None => {
                    test_debug!("Failed to parse input");
                    return None;
                }
                Some(i) => {
                    ret.push(i);
                }
            };
        }
        Some(ret)
    }

    /// Parse a transaction's inputs into raw burnchain tx inputs.
    /// (this is the behavior of Stacks 2.1 and later)
    fn parse_inputs_raw(tx: &Transaction) -> Vec<BitcoinTxInput> {
        let mut ret = vec![];
        for inp in &tx.input {
            ret.push(BitcoinTxInput::from_bitcoin_txin_raw(&inp));
        }
        ret
    }

    /// Parse a transaction's outputs into burnchain tx outputs.
    /// Does not parse the first output -- this is the OP_RETURN
    fn parse_outputs(
        &self,
        tx: &Transaction,
        epoch_id: StacksEpochId,
    ) -> Option<Vec<BitcoinTxOutput>> {
        if tx.output.is_empty() {
            return None;
        }

        let mut ret = vec![];
        for outp in &tx.output[1..tx.output.len()] {
            let out_opt = if BitcoinBlockParser::allow_segwit_outputs(epoch_id) {
                BitcoinTxOutput::from_bitcoin_txout(self.network_id, &outp)
            } else {
                BitcoinTxOutput::from_bitcoin_txout_legacy(self.network_id, &outp)
            };
            match out_opt {
                None => {
                    test_debug!("Failed to parse output");
                    return None;
                }
                Some(o) => {
                    ret.push(o);
                }
            };
        }
        Some(ret)
    }

    /// Parse a Bitcoin transaction into a Burnchain transaction.
    /// If `self.allow_raw_inputs()` is true, then scriptSigs will not be decoded.
    /// Otherwise, they will be; if decoding fails, None will be returned.
    /// In all cases, attempt to decode scriptPubKeys (and if this fails, return None)
    pub fn parse_tx(
        &self,
        tx: &Transaction,
        vtxindex: usize,
        epoch_id: StacksEpochId,
    ) -> Option<BitcoinTransaction> {
        if !self.maybe_burnchain_tx(tx, epoch_id) {
            test_debug!("Not a burnchain tx");
            return None;
        }

        let data_opt = self.parse_data(&tx.output[0].script_pubkey);
        if data_opt.is_none() {
            test_debug!("No OP_RETURN script");
            return None;
        }

        let data_amt = tx.output[0].value;

        let (opcode, data) = data_opt.unwrap();
        let inputs_opt = if BitcoinBlockParser::allow_raw_inputs(epoch_id) {
            Some(BitcoinBlockParser::parse_inputs_raw(tx))
        } else {
            BitcoinBlockParser::parse_inputs_structured(tx)
        };
        let outputs_opt = self.parse_outputs(tx, epoch_id);

        match (inputs_opt, outputs_opt) {
            (Some(inputs), Some(outputs)) => {
                Some(BitcoinTransaction {
                    txid: Txid::from_vec_be(&tx.txid().as_bytes().to_vec()).unwrap(), // this *should* panic if it fails
                    vtxindex: vtxindex as u32,
                    opcode,
                    data,
                    data_amt,
                    inputs,
                    outputs,
                })
            }
            (_, _) => {
                test_debug!("Failed to parse inputs and/or outputs");
                None
            }
        }
    }

    /// Given a Bitcoin block, extract the transactions that have OP_RETURN <magic>.
    /// Uses the internal epoch id to determine whether or not to parse segwit outputs, and whether
    /// or not to decode scriptSigs.
    pub fn parse_block(
        &self,
        block: &Block,
        block_height: u64,
        epoch_id: StacksEpochId,
    ) -> BitcoinBlock {
        let mut accepted_txs = vec![];
        for i in 0..block.txdata.len() {
            let tx = &block.txdata[i];
            match self.parse_tx(tx, i, epoch_id) {
                Some(bitcoin_tx) => {
                    accepted_txs.push(bitcoin_tx);
                }
                None => {
                    continue;
                }
            }
        }

        BitcoinBlock {
            block_height,
            block_hash: BurnchainHeaderHash::from_bitcoin_hash(&block.bitcoin_hash()),
            parent_block_hash: BurnchainHeaderHash::from_bitcoin_hash(&block.header.prev_blockhash),
            txs: accepted_txs,
            timestamp: block.header.time as u64,
        }
    }

    /// Return true if we handled the block, and we can receive the next one.  Update internal
    /// state, extract the BitcoinTransactions we care about
    ///
    /// Return false if the block we got did not match the next expected block's header
    /// (in which case, we should re-start the conversation with the peer and try again).
    pub fn process_block(
        &self,
        block: &Block,
        header: &LoneBlockHeader,
        height: u64,
        epoch_id: StacksEpochId,
    ) -> Option<BitcoinBlock> {
        // block header contents must match
        if !BitcoinBlockParser::check_block(block, header) {
            error!(
                "Expected block {} does not match received block {}",
                header.header.bitcoin_hash(),
                block.bitcoin_hash()
            );
            return None;
        }

        // parse it
        let burn_block = self.parse_block(&block, height, epoch_id);
        Some(burn_block)
    }
}

impl BurnchainBlockParser for BitcoinBlockParser {
    type D = BitcoinBlockDownloader;

    fn parse(
        &mut self,
        ipc_block: &BitcoinBlockIPC,
        epoch_id: StacksEpochId,
    ) -> Result<BurnchainBlock, burnchain_error> {
        match ipc_block.block_message {
            btc_message::NetworkMessage::Block(ref block) => {
                match self.process_block(
                    &block,
                    &ipc_block.header_data.block_header,
                    ipc_block.header_data.block_height,
                    epoch_id,
                ) {
                    None => Err(burnchain_error::ParseError),
                    Some(block_data) => Ok(BurnchainBlock::Bitcoin(block_data)),
                }
            }
            _ => {
                panic!("Did not receive a Block message"); // should never happen
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use stacks_common::deps_common::bitcoin::blockdata::block::{Block, LoneBlockHeader};
    use stacks_common::deps_common::bitcoin::blockdata::transaction::Transaction;
    use stacks_common::deps_common::bitcoin::network::encodable::VarInt;
    use stacks_common::deps_common::bitcoin::network::serialize::deserialize;
    use stacks_common::types::chainstate::BurnchainHeaderHash;
    use stacks_common::types::Address;
    use stacks_common::util::hash::hex_bytes;
    use stacks_common::util::log;

    use super::BitcoinBlockParser;
    use crate::burnchains::bitcoin::address::{BitcoinAddress, LegacyBitcoinAddressType};
    use crate::burnchains::bitcoin::keys::BitcoinPublicKey;
    use crate::burnchains::bitcoin::{
        BitcoinBlock, BitcoinInputType, BitcoinNetworkType, BitcoinTransaction, BitcoinTxInput,
        BitcoinTxInputRaw, BitcoinTxInputStructured, BitcoinTxOutput,
    };
    use crate::burnchains::{BurnchainBlock, BurnchainTransaction, MagicBytes, Txid};
    use crate::core::StacksEpochId;

    struct TxFixture {
        txstr: String,
        result: Option<BitcoinTransaction>,
    }

    struct TxParseFixture {
        txstr: String,
        result: bool,
    }

    struct BlockFixture {
        block: String,
        header: String,
        height: u64,
        result: Option<BitcoinBlock>,
    }

    fn make_tx(hex_str: &str) -> Result<Transaction, &'static str> {
        let tx_bin = hex_bytes(hex_str).map_err(|_e| "failed to decode hex")?;
        let tx = deserialize(&tx_bin.to_vec()).map_err(|_e| "failed to deserialize")?;
        Ok(tx)
    }

    fn make_block(hex_str: &str) -> Result<Block, &'static str> {
        let block_bin = hex_bytes(hex_str).map_err(|_e| "failed to decode hex")?;
        let block = deserialize(&block_bin.to_vec()).map_err(|_e| "failed to deserialize block")?;
        Ok(block)
    }

    fn make_block_header(hex_str: &str) -> Result<LoneBlockHeader, &'static str> {
        let header_bin = hex_bytes(hex_str).map_err(|_e| "failed to decode hex")?;
        let header =
            deserialize(&header_bin.to_vec()).map_err(|_e| "failed to deserialize header")?;
        Ok(LoneBlockHeader {
            header,
            tx_count: VarInt(0),
        })
    }

    fn to_txid(inp: &[u8]) -> Txid {
        let mut ret = [0; 32];
        let bytes = &inp[..inp.len()];
        ret.copy_from_slice(bytes);
        Txid(ret)
    }

    fn to_block_hash(inp: &[u8]) -> BurnchainHeaderHash {
        let mut ret = [0; 32];
        let bytes = &inp[..inp.len()];
        ret.copy_from_slice(bytes);
        BurnchainHeaderHash(ret)
    }

    #[test]
    fn maybe_burnchain_tx_test_2_05() {
        let tx_fixtures = vec![
            TxParseFixture {
                // valid
                txstr: "010000000320a081bcd1a80d9c1945f863d29dc84278411ed74cb6dcba30541bf8d5770542020000008b483045022100be57031bf2c095945ba2876e97b3f86ee051643a29b908f22ed45ccf58620103022061e056e5f48c5a51c66604a1ca28e4bfaabab1478424c9bbb396cc6afe5c222e0141040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0feffffff20a081bcd1a80d9c1945f863d29dc84278411ed74cb6dcba30541bf8d5770542010000008b483045022100fd9c04b330810694cb4bfef793b193f9cbfaa07325700f217b9cb03e5207005302202f07e7c9c6774c5619a043752444f6da6fd81b9d9d008ec965796d87271598de0141040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0feffffff20a081bcd1a80d9c1945f863d29dc84278411ed74cb6dcba30541bf8d5770542040000008a47304402205e24943a40b8ef876cc218a7e8994f4be7afb7aa02403bb73510fac01b33ead3022033e5fb811c396b2fb50a825cd1d86e82eb83483901a1793d0eb15e3e9f1d1c5b814104c77f262dda02580d65c9069a8a34c56bd77325bba4110b693b90216f5a3edc0bebc8ce28d61aa86b414aa91ecb29823b11aeed06098fcd97fee4bc73d54b1e96feffffff030000000000000000296a2769642bfae543ff5672fb607fe15e16b1c3ef38737c631c7c5d911c6617993c21fba731363f1cfe6c6b0000000000001976a914395f3643cea07ec4eec73b4d9a973dcce56b9bf188acc5120100000000001976a9149f2660e75380675206b6f1e2b4f106ae33266be488ac00000000".to_owned(),
                result: true
            },
            TxParseFixture {
                // invalid magic
                txstr: "0100000001d8b97932f097b9fbf0c7584f29515862911ac830826fdfd72d06402c21543e38000000006a47304402202801bc5d11eefddc586b1171bf607cc2be1c661d22e215153f2630316f973a200220628cc08858bba3f0cda661dbef2f007e48f8cb531edc0b54edb573226816f253012103d6967618e0159c9bfcd03ea33d368c8b2a98af5a054364c6b5e7215d7d809169ffffffff030000000000000000356a336469240efa29f955c6ae3bb5037039d89dba5e00000000000000000000000000535441434b5300000000000003e854455354217c150000000000001976a914cfd25e09f2d33e1aec73bfcc5b608ec513bbe6c088ac34460200000000001976a9144cb912533a6935880df7647fd5232e40aca07b8088ac00000000".to_owned(),
                result: false
            },
            TxParseFixture {
                // no OP_RETURN 
                txstr: "0200000003620f7bc1087b0111f76978ef747001e3ae0a12f254cbfb858f028f891c40e5f6010000006a47304402207f5dfc2f7f7329b7cc731df605c83aa6f48ec2218495324bb4ab43376f313b840220020c769655e4bfcc54e55104f6adc723867d9d819266d27e755e098f646f689d0121038c2d1cbe4d731c69e67d16c52682e01cb70b046ead63e90bf793f52f541dafbdfefffffff15fe7d9e0815853738ce47deadee69339e027a1dfcfb6fa887cce3a72626e7b010000006a47304402203202e6c640c063989623fc782ac1c9dc3c6fcaed996d852ec876749ba63db63b02207ef86e262ad4b4bc9cebfadb609f52c35b0105e15d58a5ecbecc5e536d3a8cd8012103dc526ca188418ab128d998bf80942d66f1b3be585d0c89bd61c533bddbdaa729feffffff84e6431db86833897bab333d844486c183dd01e69862edea442e480c2d8cb549010000006a47304402200320bc83f35ceab4a7ef0f8181eedb5f54e3f617626826cc49c8c86efc9be0b302203705889d6aed50f716b81b0f3f5769d72d1b8a6b59d1b0b73bcf94245c283b8001210263591c21ce8ee0d96a617108d7c278e2e715ac6d8afd3fcd158bee472c590068feffffff02ca780a00000000001976a914811fb695e46e2386501bcd70e5c869fe6c0bb33988ac10f59600000000001976a9140f2408a811f6d24ab1833924d98d884c44ecee8888ac6fce0700".to_owned(),
                result: false
            }
        ];

        let parser = BitcoinBlockParser::new(BitcoinNetworkType::Testnet, MagicBytes([105, 100])); // "id"
        for tx_fixture in tx_fixtures {
            let tx = make_tx(&tx_fixture.txstr).unwrap();
            let res = parser.maybe_burnchain_tx(&tx, StacksEpochId::Epoch2_05);
            assert_eq!(res, tx_fixture.result);
        }
    }

    /// Parse transactions in epoch 2.05 and earlier.
    /// All inputs should be BittcoinTxInputStructured
    #[test]
    fn parse_tx_test_2_05() {
        let vtxindex = 4;
        let tx_fixtures = vec![
            TxFixture {
                // NAME_UPDATE transaction with 3 singlesig inputs
                txstr: "010000000320a081bcd1a80d9c1945f863d29dc84278411ed74cb6dcba30541bf8d5770542020000008b483045022100be57031bf2c095945ba2876e97b3f86ee051643a29b908f22ed45ccf58620103022061e056e5f48c5a51c66604a1ca28e4bfaabab1478424c9bbb396cc6afe5c222e0141040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0feffffff20a081bcd1a80d9c1945f863d29dc84278411ed74cb6dcba30541bf8d5770542010000008b483045022100fd9c04b330810694cb4bfef793b193f9cbfaa07325700f217b9cb03e5207005302202f07e7c9c6774c5619a043752444f6da6fd81b9d9d008ec965796d87271598de0141040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0feffffff20a081bcd1a80d9c1945f863d29dc84278411ed74cb6dcba30541bf8d5770542040000008a47304402205e24943a40b8ef876cc218a7e8994f4be7afb7aa02403bb73510fac01b33ead3022033e5fb811c396b2fb50a825cd1d86e82eb83483901a1793d0eb15e3e9f1d1c5b814104c77f262dda02580d65c9069a8a34c56bd77325bba4110b693b90216f5a3edc0bebc8ce28d61aa86b414aa91ecb29823b11aeed06098fcd97fee4bc73d54b1e96feffffff030000000000000000296a2769642bfae543ff5672fb607fe15e16b1c3ef38737c631c7c5d911c6617993c21fba731363f1cfe6c6b0000000000001976a914395f3643cea07ec4eec73b4d9a973dcce56b9bf188acc5120100000000001976a9149f2660e75380675206b6f1e2b4f106ae33266be488ac00000000".to_owned(),
                result: Some(BitcoinTransaction {
                    data_amt: 0,
                    txid: to_txid(&hex_bytes("185c112401590b11acdfea6bb26d2a8e37cb31f24a0c89dbb8cc14b3d6271fb1").unwrap()),
                    vtxindex,
                    opcode: b'+',
                    data: hex_bytes("fae543ff5672fb607fe15e16b1c3ef38737c631c7c5d911c6617993c21fba731363f1cfe").unwrap(),
                    inputs: vec![
                        BitcoinTxInputStructured {
                            keys: vec![
                                BitcoinPublicKey::from_hex("040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0").unwrap(),
                            ],
                            num_required: 1,
                            in_type: BitcoinInputType::Standard,
                            tx_ref: (Txid::from_hex("420577d5f81b5430badcb64cd71e417842c89dd263f845199c0da8d1bc81a020").unwrap(), 2),
                        }.into(),
                        BitcoinTxInputStructured {
                            keys: vec![
                                BitcoinPublicKey::from_hex("040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0").unwrap(),
                            ],
                            num_required: 1,
                            in_type: BitcoinInputType::Standard,
                            tx_ref: (Txid::from_hex("420577d5f81b5430badcb64cd71e417842c89dd263f845199c0da8d1bc81a020").unwrap(), 1),
                        }.into(),
                        BitcoinTxInputStructured {
                            keys: vec![
                                BitcoinPublicKey::from_hex("04c77f262dda02580d65c9069a8a34c56bd77325bba4110b693b90216f5a3edc0bebc8ce28d61aa86b414aa91ecb29823b11aeed06098fcd97fee4bc73d54b1e96").unwrap(),
                            ],
                            num_required: 1,
                            in_type: BitcoinInputType::Standard,
                            tx_ref: (Txid::from_hex("420577d5f81b5430badcb64cd71e417842c89dd263f845199c0da8d1bc81a020").unwrap(), 4),
                        }.into()
                    ],
                    outputs: vec![
                        BitcoinTxOutput {
                            units: 27500,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("395f3643cea07ec4eec73b4d9a973dcce56b9bf1").unwrap()).unwrap()
                        },
                        BitcoinTxOutput {
                            units: 70341,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("9f2660e75380675206b6f1e2b4f106ae33266be4").unwrap()).unwrap()
                        }
                    ]
                })
            },
            TxFixture {
                // NAME_REVOKE with 2 2-of-3 multisig inputs
                txstr: "0100000002b4c2c2fede361654f0f6b65dd8ba385f3a4b05c76cd573f3689b09b7298b142201000000fd5c010047304402203537b5ded3716553b6f3fc7ccc7e55bc42b6caa1c069c9b2ce068d57f9024de7022026eb81e226b0de30448732835424eef52a3b9d67020c62b48df75974c5fe09870147304402201cc22e43302688d975df3bcad70065c8dad497b092a58e97c6c306b65176c70802200b9c3a62b22865e957331578d6e5d684cad87279fd8b852fcc2d34d3911e8643014cc9524104ff897d48c25c48c598aea0d6b1e835008e6679bddbc8d41d7d9f73e6a0dc2b8fe1402487ce2ba1e5365ee28fed024093499c11b8485fb6758a357c75730557674104f9478048ce8ff9cfc188a184c8c8c0a3e3dee68f96f6c3bc6f0f7e043ca8d241d5a03ab50157422ad43e9ee1a0a80b0dd17f0b0023f891dbd85daa3069554e2b41046fd8c7330fbe307a0fad0bf9472ca080f4941f4b6edea7ab090e3e26075e7277a0bd61f42eff54daf3e6141de46a98a5a8265c9e8d58bd1a86cf36d418788ab853aeffffffffb4c2c2fede361654f0f6b65dd8ba385f3a4b05c76cd573f3689b09b7298b142202000000fd5d0100473044022070cfd1e13d9844db995111ed5cc0578ca4d03504fdec1cf1636cd0054dffeeed022046c8d87291367402f4b54c2ef985a0171e400fe079da5234c912103cf2dd683b0148304502210099f092b12000dc78074934135443656091c606b40c7925bae30a6285946e36b9022062b5fa5e28986e0c27aad11f8fdb1409eb87a169972dc1ebbd91aa45810f9d9a014cc95241046097f22211c1f4832e54f0cc76c06b80a4e1fcf237ea487561c80bd5b28b6a483706a04d99038cb434eee82306902193e7b1a368dba33ad14b3f30e004c95e6e41048e264f76559020fdf50d3a7d9f57ccd548f3dfb962837f958e446add48429951d61e99a109cded2ba9812ee152ebba53a2a6c7b6dfb3c61fcba1b0852928374941044c9f30b4546c1f30087001fa6450e52c645bd49e91a18c9c16965b72f5153f0e4b04712218b42b2bc578017b471beaa7d8c0a9eb69174ad50714d7ef4117863d53aeffffffff030000000000000000176a1569647e7061747269636b7374616e6c6579322e6964f82a00000000000017a914eb1881fb0682c2eb37e478bf918525a2c61bc404876dbd13000000000017a914c26afc6cb80ca477c280780902b40cbef8cd804d8700000000".to_owned(),
                result: Some(BitcoinTransaction {
                    data_amt: 0,
                    txid: to_txid(&hex_bytes("eb2e84a45cf411e528185a98cd5fb45ed349843a83d39fd4dff2de47adad8c8f").unwrap()),
                    vtxindex,
                    opcode: b'~',
                    data: hex_bytes("7061747269636b7374616e6c6579322e6964").unwrap(),
                    inputs: vec![
                        BitcoinTxInputStructured {
                            keys: vec![
                                BitcoinPublicKey::from_hex("04ff897d48c25c48c598aea0d6b1e835008e6679bddbc8d41d7d9f73e6a0dc2b8fe1402487ce2ba1e5365ee28fed024093499c11b8485fb6758a357c7573055767").unwrap(),
                                BitcoinPublicKey::from_hex("04f9478048ce8ff9cfc188a184c8c8c0a3e3dee68f96f6c3bc6f0f7e043ca8d241d5a03ab50157422ad43e9ee1a0a80b0dd17f0b0023f891dbd85daa3069554e2b").unwrap(),
                                BitcoinPublicKey::from_hex("046fd8c7330fbe307a0fad0bf9472ca080f4941f4b6edea7ab090e3e26075e7277a0bd61f42eff54daf3e6141de46a98a5a8265c9e8d58bd1a86cf36d418788ab8").unwrap(),
                            ],
                            num_required: 2,
                            in_type: BitcoinInputType::Standard,
                            tx_ref: (Txid::from_hex("22148b29b7099b68f373d56cc7054b3a5f38bad85db6f6f0541636defec2c2b4").unwrap(), 1),
                        }.into(),
                        BitcoinTxInputStructured {
                            keys: vec![
                                BitcoinPublicKey::from_hex("046097f22211c1f4832e54f0cc76c06b80a4e1fcf237ea487561c80bd5b28b6a483706a04d99038cb434eee82306902193e7b1a368dba33ad14b3f30e004c95e6e").unwrap(),
                                BitcoinPublicKey::from_hex("048e264f76559020fdf50d3a7d9f57ccd548f3dfb962837f958e446add48429951d61e99a109cded2ba9812ee152ebba53a2a6c7b6dfb3c61fcba1b08529283749").unwrap(),
                                BitcoinPublicKey::from_hex("044c9f30b4546c1f30087001fa6450e52c645bd49e91a18c9c16965b72f5153f0e4b04712218b42b2bc578017b471beaa7d8c0a9eb69174ad50714d7ef4117863d").unwrap(),
                            ],
                            num_required: 2,
                            in_type: BitcoinInputType::Standard,
                            tx_ref: (Txid::from_hex("22148b29b7099b68f373d56cc7054b3a5f38bad85db6f6f0541636defec2c2b4").unwrap(), 2),
                        }.into(),
                    ],
                    outputs: vec![
                        BitcoinTxOutput {
                            units: 11000,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("eb1881fb0682c2eb37e478bf918525a2c61bc404").unwrap()).unwrap()
                        },
                        BitcoinTxOutput {
                            units: 1293677,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("c26afc6cb80ca477c280780902b40cbef8cd804d").unwrap()).unwrap()
                        }
                    ]
                })
            },
            TxFixture {
                // NAME_REGISTRATION with p2wpkh-p2sh segwit input
                txstr: "01000000000101a7ef2b09722ad786c569c0812005a731ce19290bb0a2afc16cb91056c2e4c19e0100000017160014393ffec4f09b38895b8502377693f23c6ae00f19ffffffff0300000000000000000d6a0b69643a666f6f2e746573747c1500000000000017a9144b85301ba8e42bf98472b8ed4939d5f76b98fcea87144d9c290100000017a91431f8968eb1730c83fb58409a9a560a0a0835027f8702483045022100fc82815edf1c0ef0c601cf1e26494626d7b01597be5ab83df025ff1ee67730130220016c4c29d77aadb5ff57c0c9272a43950ca29b84d8adfaed95ac69db90b35d5b012102d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e8100000000".to_owned(),
                result: Some(BitcoinTransaction {
                    data_amt: 0,
                    txid: to_txid(&hex_bytes("b908952b30ccfdfa59985dc1ffdd2a22ef054d20fa253510d2af7797dddee459").unwrap()),
                    vtxindex,
                    opcode: b':',
                    data: hex_bytes("666f6f2e74657374").unwrap(),
                    inputs: vec![
                        BitcoinTxInputStructured {
                            keys: vec![
                                BitcoinPublicKey::from_hex("02d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e81").unwrap()
                            ],
                            num_required: 1,
                            in_type: BitcoinInputType::SegwitP2SH,
                            tx_ref: (Txid::from_hex("9ec1e4c25610b96cc1afa2b00b2919ce31a7052081c069c586d72a72092befa7").unwrap(), 1),
                        }.into()
                    ],
                    outputs: vec![
                        BitcoinTxOutput {
                            units: 5500,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("4b85301ba8e42bf98472b8ed4939d5f76b98fcea").unwrap()).unwrap()
                        },
                        BitcoinTxOutput {
                            units: 4993076500,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("31f8968eb1730c83fb58409a9a560a0a0835027f").unwrap()).unwrap()
                        }
                    ]
                })
            },
            TxFixture {
                // NAME_PREORDER with a 2-of-3 p2wsh-p2sh multisig segwit input 
                txstr: "01000000000101e411dc967b8503a27450c614a5cd984698762a6b4bf547293ffdf846ed4ebd22010000002322002067091a41e9871c5ae20b0c69a786f02df5d3c7aa632689b608069181b43a28a2ffffffff030000000000000000296a2769643f9fab7f294936ddb6524a48feff691ecbd0ca9e8f107d845c417a5438d1cb441e827c5126b01ba0290100000017a91487a0487869af70b6b1cc79bd374b75ba1be5cff98700a86100000000001976a914000000000000000000000000000000000000000088ac0400473044022064c5b5f61baad8bb8ecad98666b99e09f1777ef805df41a1c7926f8468b6b6df02205eac177c77f274acb670cd24d504f01b27de767e0241c818c91e479cb0ddcf18014730440220053ce777bc7bb842d8eef83769a027797567624ab9eed5722889ed3192f431b30220256e8aaef8de2a571198acde708fcbca02fb18780ac470c0d7f811734af729af0169522102d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e812102f21b29694df4c2188bee97103d10d017d1865fb40528f25589af9db6e0786b6521028791dc45c049107fb99e673265a38a096536aacdf78aa90710a32fff7750f9f953ae00000000".to_owned(),
                result: Some(BitcoinTransaction {
                    data_amt: 0,
                    txid: to_txid(&hex_bytes("16751ca54407b922e3072830cf4be58c5562a6dc350f6703192b673c4cc86182").unwrap()),
                    vtxindex,
                    opcode: b'?',
                    data: hex_bytes("9fab7f294936ddb6524a48feff691ecbd0ca9e8f107d845c417a5438d1cb441e827c5126").unwrap(),
                    inputs: vec![
                        BitcoinTxInputStructured {
                            keys: vec![
                                BitcoinPublicKey::from_hex("02d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e81").unwrap(),
                                BitcoinPublicKey::from_hex("02f21b29694df4c2188bee97103d10d017d1865fb40528f25589af9db6e0786b65").unwrap(),
                                BitcoinPublicKey::from_hex("028791dc45c049107fb99e673265a38a096536aacdf78aa90710a32fff7750f9f9").unwrap()
                            ],
                            num_required: 2,
                            in_type: BitcoinInputType::SegwitP2SH,
                            tx_ref: (Txid::from_hex("22bd4eed46f8fd3f2947f54b6b2a76984698cda514c65074a203857b96dc11e4").unwrap(), 1),
                        }.into()
                    ],
                    outputs: vec![
                        BitcoinTxOutput {
                            units: 4993326000,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("87a0487869af70b6b1cc79bd374b75ba1be5cff9").unwrap()).unwrap()
                        },
                        BitcoinTxOutput {
                            units: 6400000,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("0000000000000000000000000000000000000000").unwrap()).unwrap()
                        },
                    ]
                })
            }
        ];

        let parser = BitcoinBlockParser::new(BitcoinNetworkType::Testnet, MagicBytes([105, 100])); // "id"
        for tx_fixture in tx_fixtures {
            let tx = make_tx(&tx_fixture.txstr).unwrap();
            let burnchain_tx = parser.parse_tx(&tx, vtxindex as usize, StacksEpochId::Epoch2_05);
            assert!(burnchain_tx.is_some());
            assert_eq!(burnchain_tx, tx_fixture.result);
        }
    }

    /// Parse transactions in epoch 2.1
    /// All inputs should be BitcoinTxInputRaw
    #[test]
    fn parse_tx_test_2_1() {
        let vtxindex = 4;
        let tx_fixtures = vec![
            TxFixture {
                // NAME_UPDATE transaction with 3 singlesig inputs
                txstr: "010000000320a081bcd1a80d9c1945f863d29dc84278411ed74cb6dcba30541bf8d5770542020000008b483045022100be57031bf2c095945ba2876e97b3f86ee051643a29b908f22ed45ccf58620103022061e056e5f48c5a51c66604a1ca28e4bfaabab1478424c9bbb396cc6afe5c222e0141040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0feffffff20a081bcd1a80d9c1945f863d29dc84278411ed74cb6dcba30541bf8d5770542010000008b483045022100fd9c04b330810694cb4bfef793b193f9cbfaa07325700f217b9cb03e5207005302202f07e7c9c6774c5619a043752444f6da6fd81b9d9d008ec965796d87271598de0141040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0feffffff20a081bcd1a80d9c1945f863d29dc84278411ed74cb6dcba30541bf8d5770542040000008a47304402205e24943a40b8ef876cc218a7e8994f4be7afb7aa02403bb73510fac01b33ead3022033e5fb811c396b2fb50a825cd1d86e82eb83483901a1793d0eb15e3e9f1d1c5b814104c77f262dda02580d65c9069a8a34c56bd77325bba4110b693b90216f5a3edc0bebc8ce28d61aa86b414aa91ecb29823b11aeed06098fcd97fee4bc73d54b1e96feffffff030000000000000000296a2769642bfae543ff5672fb607fe15e16b1c3ef38737c631c7c5d911c6617993c21fba731363f1cfe6c6b0000000000001976a914395f3643cea07ec4eec73b4d9a973dcce56b9bf188acc5120100000000001976a9149f2660e75380675206b6f1e2b4f106ae33266be488ac00000000".to_owned(),
                result: Some(BitcoinTransaction {
                    data_amt: 0,
                    txid: to_txid(&hex_bytes("185c112401590b11acdfea6bb26d2a8e37cb31f24a0c89dbb8cc14b3d6271fb1").unwrap()),
                    vtxindex,
                    opcode: b'+',
                    data: hex_bytes("fae543ff5672fb607fe15e16b1c3ef38737c631c7c5d911c6617993c21fba731363f1cfe").unwrap(),
                    inputs: vec![
                        BitcoinTxInputRaw {
                            scriptSig: hex_bytes("483045022100be57031bf2c095945ba2876e97b3f86ee051643a29b908f22ed45ccf58620103022061e056e5f48c5a51c66604a1ca28e4bfaabab1478424c9bbb396cc6afe5c222e0141040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0").unwrap(),
                            witness: vec![],
                            tx_ref: (Txid::from_hex("420577d5f81b5430badcb64cd71e417842c89dd263f845199c0da8d1bc81a020").unwrap(), 2),
                        }.into(),
                        BitcoinTxInputRaw {
                            scriptSig: hex_bytes("483045022100fd9c04b330810694cb4bfef793b193f9cbfaa07325700f217b9cb03e5207005302202f07e7c9c6774c5619a043752444f6da6fd81b9d9d008ec965796d87271598de0141040fadbbcea0ff3b05f03195b41cd991d7a0af8bd38559943aec99cbdaf0b22cc806b9a4f07579934774cc0c155e781d45c989f94336765e88a66d91cfb9f060b0").unwrap(),
                            witness: vec![],
                            tx_ref: (Txid::from_hex("420577d5f81b5430badcb64cd71e417842c89dd263f845199c0da8d1bc81a020").unwrap(), 1),
                        }.into(),
                        BitcoinTxInputRaw {
                            scriptSig: hex_bytes("47304402205e24943a40b8ef876cc218a7e8994f4be7afb7aa02403bb73510fac01b33ead3022033e5fb811c396b2fb50a825cd1d86e82eb83483901a1793d0eb15e3e9f1d1c5b814104c77f262dda02580d65c9069a8a34c56bd77325bba4110b693b90216f5a3edc0bebc8ce28d61aa86b414aa91ecb29823b11aeed06098fcd97fee4bc73d54b1e96").unwrap(),
                            witness: vec![],
                            tx_ref: (Txid::from_hex("420577d5f81b5430badcb64cd71e417842c89dd263f845199c0da8d1bc81a020").unwrap(), 4),
                        }.into()
                    ],
                    outputs: vec![
                        BitcoinTxOutput {
                            units: 27500,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Mainnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("395f3643cea07ec4eec73b4d9a973dcce56b9bf1").unwrap()).unwrap()
                        },
                        BitcoinTxOutput {
                            units: 70341,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Mainnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("9f2660e75380675206b6f1e2b4f106ae33266be4").unwrap()).unwrap()
                        }
                    ]
                })
            },
            TxFixture {
                // NAME_REVOKE with 2 2-of-3 multisig inputs
                txstr: "0100000002b4c2c2fede361654f0f6b65dd8ba385f3a4b05c76cd573f3689b09b7298b142201000000fd5c010047304402203537b5ded3716553b6f3fc7ccc7e55bc42b6caa1c069c9b2ce068d57f9024de7022026eb81e226b0de30448732835424eef52a3b9d67020c62b48df75974c5fe09870147304402201cc22e43302688d975df3bcad70065c8dad497b092a58e97c6c306b65176c70802200b9c3a62b22865e957331578d6e5d684cad87279fd8b852fcc2d34d3911e8643014cc9524104ff897d48c25c48c598aea0d6b1e835008e6679bddbc8d41d7d9f73e6a0dc2b8fe1402487ce2ba1e5365ee28fed024093499c11b8485fb6758a357c75730557674104f9478048ce8ff9cfc188a184c8c8c0a3e3dee68f96f6c3bc6f0f7e043ca8d241d5a03ab50157422ad43e9ee1a0a80b0dd17f0b0023f891dbd85daa3069554e2b41046fd8c7330fbe307a0fad0bf9472ca080f4941f4b6edea7ab090e3e26075e7277a0bd61f42eff54daf3e6141de46a98a5a8265c9e8d58bd1a86cf36d418788ab853aeffffffffb4c2c2fede361654f0f6b65dd8ba385f3a4b05c76cd573f3689b09b7298b142202000000fd5d0100473044022070cfd1e13d9844db995111ed5cc0578ca4d03504fdec1cf1636cd0054dffeeed022046c8d87291367402f4b54c2ef985a0171e400fe079da5234c912103cf2dd683b0148304502210099f092b12000dc78074934135443656091c606b40c7925bae30a6285946e36b9022062b5fa5e28986e0c27aad11f8fdb1409eb87a169972dc1ebbd91aa45810f9d9a014cc95241046097f22211c1f4832e54f0cc76c06b80a4e1fcf237ea487561c80bd5b28b6a483706a04d99038cb434eee82306902193e7b1a368dba33ad14b3f30e004c95e6e41048e264f76559020fdf50d3a7d9f57ccd548f3dfb962837f958e446add48429951d61e99a109cded2ba9812ee152ebba53a2a6c7b6dfb3c61fcba1b0852928374941044c9f30b4546c1f30087001fa6450e52c645bd49e91a18c9c16965b72f5153f0e4b04712218b42b2bc578017b471beaa7d8c0a9eb69174ad50714d7ef4117863d53aeffffffff030000000000000000176a1569647e7061747269636b7374616e6c6579322e6964f82a00000000000017a914eb1881fb0682c2eb37e478bf918525a2c61bc404876dbd13000000000017a914c26afc6cb80ca477c280780902b40cbef8cd804d8700000000".to_owned(),
                result: Some(BitcoinTransaction {
                    data_amt: 0,
                    txid: to_txid(&hex_bytes("eb2e84a45cf411e528185a98cd5fb45ed349843a83d39fd4dff2de47adad8c8f").unwrap()),
                    vtxindex,
                    opcode: b'~',
                    data: hex_bytes("7061747269636b7374616e6c6579322e6964").unwrap(),
                    inputs: vec![
                        BitcoinTxInputRaw {
                            scriptSig: hex_bytes("0047304402203537b5ded3716553b6f3fc7ccc7e55bc42b6caa1c069c9b2ce068d57f9024de7022026eb81e226b0de30448732835424eef52a3b9d67020c62b48df75974c5fe09870147304402201cc22e43302688d975df3bcad70065c8dad497b092a58e97c6c306b65176c70802200b9c3a62b22865e957331578d6e5d684cad87279fd8b852fcc2d34d3911e8643014cc9524104ff897d48c25c48c598aea0d6b1e835008e6679bddbc8d41d7d9f73e6a0dc2b8fe1402487ce2ba1e5365ee28fed024093499c11b8485fb6758a357c75730557674104f9478048ce8ff9cfc188a184c8c8c0a3e3dee68f96f6c3bc6f0f7e043ca8d241d5a03ab50157422ad43e9ee1a0a80b0dd17f0b0023f891dbd85daa3069554e2b41046fd8c7330fbe307a0fad0bf9472ca080f4941f4b6edea7ab090e3e26075e7277a0bd61f42eff54daf3e6141de46a98a5a8265c9e8d58bd1a86cf36d418788ab853ae").unwrap(),
                            witness: vec![],
                            tx_ref: (Txid::from_hex("22148b29b7099b68f373d56cc7054b3a5f38bad85db6f6f0541636defec2c2b4").unwrap(), 1),
                        }.into(),
                        BitcoinTxInputRaw {
                            scriptSig: hex_bytes("00473044022070cfd1e13d9844db995111ed5cc0578ca4d03504fdec1cf1636cd0054dffeeed022046c8d87291367402f4b54c2ef985a0171e400fe079da5234c912103cf2dd683b0148304502210099f092b12000dc78074934135443656091c606b40c7925bae30a6285946e36b9022062b5fa5e28986e0c27aad11f8fdb1409eb87a169972dc1ebbd91aa45810f9d9a014cc95241046097f22211c1f4832e54f0cc76c06b80a4e1fcf237ea487561c80bd5b28b6a483706a04d99038cb434eee82306902193e7b1a368dba33ad14b3f30e004c95e6e41048e264f76559020fdf50d3a7d9f57ccd548f3dfb962837f958e446add48429951d61e99a109cded2ba9812ee152ebba53a2a6c7b6dfb3c61fcba1b0852928374941044c9f30b4546c1f30087001fa6450e52c645bd49e91a18c9c16965b72f5153f0e4b04712218b42b2bc578017b471beaa7d8c0a9eb69174ad50714d7ef4117863d53ae").unwrap(),
                            witness: vec![],
                            tx_ref: (Txid::from_hex("22148b29b7099b68f373d56cc7054b3a5f38bad85db6f6f0541636defec2c2b4").unwrap(), 2),
                        }.into(),
                    ],
                    outputs: vec![
                        BitcoinTxOutput {
                            units: 11000,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Mainnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("eb1881fb0682c2eb37e478bf918525a2c61bc404").unwrap()).unwrap()
                        },
                        BitcoinTxOutput {
                            units: 1293677,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Mainnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("c26afc6cb80ca477c280780902b40cbef8cd804d").unwrap()).unwrap()
                        }
                    ]
                })
            },
            TxFixture {
                // NAME_REGISTRATION with p2wpkh-p2sh segwit input
                txstr: "01000000000101a7ef2b09722ad786c569c0812005a731ce19290bb0a2afc16cb91056c2e4c19e0100000017160014393ffec4f09b38895b8502377693f23c6ae00f19ffffffff0300000000000000000d6a0b69643a666f6f2e746573747c1500000000000017a9144b85301ba8e42bf98472b8ed4939d5f76b98fcea87144d9c290100000017a91431f8968eb1730c83fb58409a9a560a0a0835027f8702483045022100fc82815edf1c0ef0c601cf1e26494626d7b01597be5ab83df025ff1ee67730130220016c4c29d77aadb5ff57c0c9272a43950ca29b84d8adfaed95ac69db90b35d5b012102d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e8100000000".to_owned(),
                result: Some(BitcoinTransaction {
                    data_amt: 0,
                    txid: to_txid(&hex_bytes("b908952b30ccfdfa59985dc1ffdd2a22ef054d20fa253510d2af7797dddee459").unwrap()),
                    vtxindex,
                    opcode: b':',
                    data: hex_bytes("666f6f2e74657374").unwrap(),
                    inputs: vec![
                        BitcoinTxInputRaw {
                            scriptSig: hex_bytes("160014393ffec4f09b38895b8502377693f23c6ae00f19").unwrap(),
                            witness: vec![
                                hex_bytes("3045022100fc82815edf1c0ef0c601cf1e26494626d7b01597be5ab83df025ff1ee67730130220016c4c29d77aadb5ff57c0c9272a43950ca29b84d8adfaed95ac69db90b35d5b01").unwrap(),
                                hex_bytes("02d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e81").unwrap()
                            ],
                            tx_ref: (Txid::from_hex("9ec1e4c25610b96cc1afa2b00b2919ce31a7052081c069c586d72a72092befa7").unwrap(), 1),
                        }.into(),
                    ],
                    outputs: vec![
                        BitcoinTxOutput {
                            units: 5500,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Mainnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("4b85301ba8e42bf98472b8ed4939d5f76b98fcea").unwrap()).unwrap()
                        },
                        BitcoinTxOutput {
                            units: 4993076500,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Mainnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("31f8968eb1730c83fb58409a9a560a0a0835027f").unwrap()).unwrap()
                        }
                    ]
                })
            },
            TxFixture {
                // NAME_PREORDER with a 2-of-3 p2wsh-p2sh multisig segwit input 
                txstr: "01000000000101e411dc967b8503a27450c614a5cd984698762a6b4bf547293ffdf846ed4ebd22010000002322002067091a41e9871c5ae20b0c69a786f02df5d3c7aa632689b608069181b43a28a2ffffffff030000000000000000296a2769643f9fab7f294936ddb6524a48feff691ecbd0ca9e8f107d845c417a5438d1cb441e827c5126b01ba0290100000017a91487a0487869af70b6b1cc79bd374b75ba1be5cff98700a86100000000001976a914000000000000000000000000000000000000000088ac0400473044022064c5b5f61baad8bb8ecad98666b99e09f1777ef805df41a1c7926f8468b6b6df02205eac177c77f274acb670cd24d504f01b27de767e0241c818c91e479cb0ddcf18014730440220053ce777bc7bb842d8eef83769a027797567624ab9eed5722889ed3192f431b30220256e8aaef8de2a571198acde708fcbca02fb18780ac470c0d7f811734af729af0169522102d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e812102f21b29694df4c2188bee97103d10d017d1865fb40528f25589af9db6e0786b6521028791dc45c049107fb99e673265a38a096536aacdf78aa90710a32fff7750f9f953ae00000000".to_owned(),
                result: Some(BitcoinTransaction {
                    data_amt: 0,
                    txid: to_txid(&hex_bytes("16751ca54407b922e3072830cf4be58c5562a6dc350f6703192b673c4cc86182").unwrap()),
                    vtxindex,
                    opcode: b'?',
                    data: hex_bytes("9fab7f294936ddb6524a48feff691ecbd0ca9e8f107d845c417a5438d1cb441e827c5126").unwrap(),
                    inputs: vec![
                        BitcoinTxInputRaw {
                            scriptSig: hex_bytes("22002067091a41e9871c5ae20b0c69a786f02df5d3c7aa632689b608069181b43a28a2").unwrap(),
                            witness: vec![
                                vec![],
                                hex_bytes("3044022064c5b5f61baad8bb8ecad98666b99e09f1777ef805df41a1c7926f8468b6b6df02205eac177c77f274acb670cd24d504f01b27de767e0241c818c91e479cb0ddcf1801").unwrap(),
                                hex_bytes("30440220053ce777bc7bb842d8eef83769a027797567624ab9eed5722889ed3192f431b30220256e8aaef8de2a571198acde708fcbca02fb18780ac470c0d7f811734af729af01").unwrap(),
                                hex_bytes("522102d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e812102f21b29694df4c2188bee97103d10d017d1865fb40528f25589af9db6e0786b6521028791dc45c049107fb99e673265a38a096536aacdf78aa90710a32fff7750f9f953ae").unwrap()
                            ],
                            tx_ref: (Txid::from_hex("22bd4eed46f8fd3f2947f54b6b2a76984698cda514c65074a203857b96dc11e4").unwrap(), 1),
                        }.into(),
                    ],
                    outputs: vec![
                        BitcoinTxOutput {
                            units: 4993326000,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Mainnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("87a0487869af70b6b1cc79bd374b75ba1be5cff9").unwrap()).unwrap()
                        },
                        BitcoinTxOutput {
                            units: 6400000,
                            address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Mainnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("0000000000000000000000000000000000000000").unwrap()).unwrap()
                        },
                    ]
                })
            },
            TxFixture {
                // NAMESPACE_REVEAL with a segwit p2wpkh script pubkey
                txstr: "0100000001fde2146ec3ecf037ad515c0c1e2ba8abee348bd2b3c6a576bf909d78b0b18cd2010000006a47304402203ec06f11bc5b7e79fad54b2d69a375ba78576a2a0293f531a082fcfe13a9e9e802201afcf0038d9ccb9c88113248faaf812321b65d7b09b4a6e2f04f463d2741101e012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3ffffffff0300000000000000001a6a186964260000cd73fa046543210000000000aa0001746573747c1500000000000016001482093b62a3699282d926981bed7665e8384caa552076fd29010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac00000000".to_owned(),
                result: Some(BitcoinTransaction {
                    data_amt: 0,
                    txid: to_txid(&hex_bytes("8b8a12909d48fd86c06e92270133d320498fb36caa0fdcb3292a8bba99669ebd").unwrap()),
                    vtxindex,
                    opcode: b'&',
                    data: hex_bytes("0000cd73fa046543210000000000aa000174657374").unwrap(),
                    inputs: vec![
                        BitcoinTxInputRaw {
                            scriptSig: hex_bytes("47304402203ec06f11bc5b7e79fad54b2d69a375ba78576a2a0293f531a082fcfe13a9e9e802201afcf0038d9ccb9c88113248faaf812321b65d7b09b4a6e2f04f463d2741101e012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3").unwrap(),
                            witness: vec![],
                            tx_ref: (Txid::from_hex("d28cb1b0789d90bf76a5c6b3d28b34eeaba82b1e0c5c51ad37f0ecc36e14e2fd").unwrap(), 1),
                        }.into()
                    ],
                    outputs: vec![
                        BitcoinTxOutput {
                            units: 5500,
                            address: BitcoinAddress::from_string("bc1qsgynkc4rdxfg9kfxnqd76an9aquye2j4kdnk7c").unwrap(),
                        },
                        BitcoinTxOutput {
                            units: 4999444000,
                            address: BitcoinAddress::from_string("1BaqZJqwt2dcdxt6oa3mwSK4DiEyfXCgnZ").unwrap(),
                        },
                    ],
                }),
            }
        ];

        let parser = BitcoinBlockParser::new(BitcoinNetworkType::Mainnet, MagicBytes([105, 100])); // "id"
        for tx_fixture in tx_fixtures {
            test_debug!("parse {}", &tx_fixture.txstr);
            let tx = make_tx(&tx_fixture.txstr).unwrap();
            let burnchain_tx = parser.parse_tx(&tx, vtxindex as usize, StacksEpochId::Epoch21);
            assert!(burnchain_tx.is_some());
            assert_eq!(burnchain_tx, tx_fixture.result);
        }
    }

    #[test]
    fn parse_tx_strange_2_05() {
        let vtxindex = 4;
        let tx_fixtures_strange : Vec<TxFixture> = vec![
            TxFixture {
                // NAMESPACE_REVEAL with a segwit p2wpkh script pubkey (shouldn't parse in epoch
                // 2.05)
                txstr: "0100000001fde2146ec3ecf037ad515c0c1e2ba8abee348bd2b3c6a576bf909d78b0b18cd2010000006a47304402203ec06f11bc5b7e79fad54b2d69a375ba78576a2a0293f531a082fcfe13a9e9e802201afcf0038d9ccb9c88113248faaf812321b65d7b09b4a6e2f04f463d2741101e012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3ffffffff0300000000000000001a6a186964260000cd73fa046543210000000000aa0001746573747c1500000000000016001482093b62a3699282d926981bed7665e8384caa552076fd29010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac00000000".to_owned(),
                result: None
            },
            TxFixture {
                // coinbase 
                txstr: "02000000010000000000000000000000000000000000000000000000000000000000000000ffffffff0502b7020101ffffffff024023b71200000000232103ecfa5bcaa0d2b7dd3a705342be2e144f66293be99488c8e5c9bc3d843036f1bfac0000000000000000266a24aa21a9ed620a2609f2f58ea62134d1c54bf73cb6e0cf194cfbdf25ae32b55dd167ee64bb00000000".to_owned(),
                result: None
            },
            // TODO: add more transactions with non-standard scripts that we don't care about
        ];

        let parser = BitcoinBlockParser::new(BitcoinNetworkType::Testnet, MagicBytes([105, 100])); // "id"
        for tx_fixture in tx_fixtures_strange {
            let tx = make_tx(&tx_fixture.txstr).unwrap();
            let burnchain_tx = parser.parse_tx(&tx, vtxindex as usize, StacksEpochId::Epoch2_05);
            assert!(burnchain_tx.is_none());
        }
    }

    #[test]
    fn parse_block() {
        let block_fixtures = vec![
            BlockFixture {
                // block with one NAME_REGISTRATION and one coinbase 
                block: "000000209cef4ccd19f4294dd5c762aab6d9577fb4412cd4c0a662a953a8b7969697bc1ddab52e6f053758022fb92f04388eb5fdd87046776e9c406880e728b48e6930aff462fc5bffff7f200000000002020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff0502b5020101ffffffff024018a41200000000232103f51f0c868fd99a4a3a14fe2153fba3c5f635c31bf0a588545627134b49609097ac0000000000000000266a24aa21a9ed18a09ae86261d6802bff7fa705afa558764ed3750c2273bfae5b5136c44d14d6012000000000000000000000000000000000000000000000000000000000000000000000000001000000000101a7ef2b09722ad786c569c0812005a731ce19290bb0a2afc16cb91056c2e4c19e0100000017160014393ffec4f09b38895b8502377693f23c6ae00f19ffffffff0300000000000000000d6a0b69643a666f6f2e746573747c1500000000000017a9144b85301ba8e42bf98472b8ed4939d5f76b98fcea87144d9c290100000017a91431f8968eb1730c83fb58409a9a560a0a0835027f8702483045022100fc82815edf1c0ef0c601cf1e26494626d7b01597be5ab83df025ff1ee67730130220016c4c29d77aadb5ff57c0c9272a43950ca29b84d8adfaed95ac69db90b35d5b012102d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e8100000000".to_owned(),
                header: "000000209cef4ccd19f4294dd5c762aab6d9577fb4412cd4c0a662a953a8b7969697bc1ddab52e6f053758022fb92f04388eb5fdd87046776e9c406880e728b48e6930aff462fc5bffff7f2000000000".to_owned(),
                height: 32,
                result: Some(BitcoinBlock {
                    block_height: 32,
                    parent_block_hash: to_block_hash(&hex_bytes("1dbc979696b7a853a962a6c0d42c41b47f57d9b6aa62c7d54d29f419cd4cef9c").unwrap()),
                    block_hash: to_block_hash(&hex_bytes("7483b1104341d596c1d0d2499cb1821b0e078329deabc4e7504c016a5b393e08").unwrap()),
                    txs: vec![
                        BitcoinTransaction {
                            data_amt: 0,
                            // NAME_REGISTRATION with segwit p2wpkh-p2sh input
                            txid: to_txid(&hex_bytes("b908952b30ccfdfa59985dc1ffdd2a22ef054d20fa253510d2af7797dddee459").unwrap()),
                            vtxindex: 1,
                            opcode: b':',
                            data: hex_bytes("666f6f2e74657374").unwrap(),
                            inputs: vec![
                                BitcoinTxInputStructured {
                                    keys: vec![
                                        BitcoinPublicKey::from_hex("02d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e81").unwrap()
                                    ],
                                    num_required: 1,
                                    in_type: BitcoinInputType::SegwitP2SH,
                                    tx_ref: (Txid::from_hex("9ec1e4c25610b96cc1afa2b00b2919ce31a7052081c069c586d72a72092befa7").unwrap(), 1),
                                }.into(),
                            ],
                            outputs: vec![
                                BitcoinTxOutput {
                                    units: 5500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("4b85301ba8e42bf98472b8ed4939d5f76b98fcea").unwrap()).unwrap()
                                },
                                BitcoinTxOutput {
                                    units: 4993076500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::ScriptHash, &hex_bytes("31f8968eb1730c83fb58409a9a560a0a0835027f").unwrap()).unwrap()
                                }
                            ]
                        }
                    ],
                    timestamp: 1543267060,
                })
            },
            BlockFixture {
                // a block with 5 TOKEN_TRANSFERs and a bunch of non-OP_RETURN transactions
                block: "00000020ad98a2888b7c69f4187ef5ee1b5921a6fb62803aa8bd35826f7fb751714baf250cb5ef03478d35ed7f6582ab40232ee39744471b2bcb40b91db0f29102d695123379fc5bffff7f20020000001402000000010000000000000000000000000000000000000000000000000000000000000000ffffffff0502b7020101ffffffff024023b71200000000232103ecfa5bcaa0d2b7dd3a705342be2e144f66293be99488c8e5c9bc3d843036f1bfac0000000000000000266a24aa21a9ed620a2609f2f58ea62134d1c54bf73cb6e0cf194cfbdf25ae32b55dd167ee64bb00000000010000000169cdf5fb51781758c7e77dc8e86c99248bb4decd3dd39ac782270b120a77d5d2000000006a47304402201db67a44a12472e8e555efbe826927fd1b67cbc5db42ba43a31edd2177fc32cc02206ee9c8f42629fbd988dc6091f46c1f7921cd58366531529ea2b91a36b2cbba9a012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff0220a10700000000001976a91441a349571d89decfac52ffecd92300b6a97b284188ac74a73729010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac000000000100000001c517ff49a374f8a41dd7a5d4028315374f875bd483a4e56bf946d76a0ec441f7010000006a473044022079f4cf76c0ce6da1c01beff79521817561f98dff27f63ce394a22fa645aa2c6502204343f6f7ed1a06e01a8b1d379ae11e5a4a6214c8056d68cde1946458960d5a46012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3ffffffff030000000000000000306a2e6964247c503a2e30a905cb515cfbc291766dfa00000000000000000000000000535441434b5300000000000000647c150000000000001976a91441a349571d89decfac52ffecd92300b6a97b284188ac80403329010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac00000000010000000128877bb55365102a106d3150dec58e23c4e38fa6d19f6cedd6d4e3bb4dc5f213020000006a4730440220515a2ad1c809a519edd17103bdb53578366b44b56983576e2bb96eab7571a54f02201b86aa94a34374e20a1d3f50d257e4da91a59e91af385bd7672f4866b2d2d65b012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff0220a10700000000001976a91441a349571d89decfac52ffecd92300b6a97b284188ace4892b29010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac0000000001000000019311d3968c1529d7c88df93518af051a28967c2e40f7a9d71581d1b3d5c153ba000000008a4730440220491ed78e9b5b6654d811d4d2586b95488b04914cdcde68ab5b3320e946fce23f02202bf2d772438b777008a0107c5ce5aff252bc96f613febb6eaa1997007c0afcb1014104ef29f16c10aa2d0468d7841cfedb8b5729689ebca4db38fb8f3fc9ab158e799b6d6dfc2bca52fe490f7acd38e351bf1d28b8f1f48736a0b022f806dd107a8385ffffffff030000000000000000306a2e6964247c503a2e30a905cb515cfbc291766dfa00000000000000000000000000535441434b5300000000000000647c150000000000001976a914e1762290e3f035ea4e7f8cbf72a9d9386c4020ab88ac2c3a0300000000001976a91441a349571d89decfac52ffecd92300b6a97b284188ac0000000001000000019311d3968c1529d7c88df93518af051a28967c2e40f7a9d71581d1b3d5c153ba010000006a4730440220707c69e458fe9e2325fd5861d66f71d79226466999a96e9c10184fd8c14830ae02207165850443badce9957b10e967bdd1e0eb6b47bb9f3e30ef4e1d7dfc762b40c6012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff0220a10700000000001976a914e1762290e3f035ea4e7f8cbf72a9d9386c4020ab88ac48d32329010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac000000000100000001ca544deb6c248c68a56d86e3e9fa2f93fcf35d6055acb116962421eb4041e896010000006a47304402206218d746f7d4b788362ba33557c1a57de815fe1d024b3a7cf2a45c12595db4a502205bc66b5817530820e5b572e9f8ce702b3c2beae89c836aac297b727b4f2fef79012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff0220a10700000000001976a914e1762290e3f035ea4e7f8cbf72a9d9386c4020ab88acac1c1c29010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac000000000100000001f433cac39fa99d6621e10148fdba962a98c0647214fb6a050c742cc423528cbb000000008b483045022100d27598ea9e8fde498f94d645e6ee805cc58c256163dd7e0bcb074b6f3498fd4d02204582fc8f281707f2229606d7c7287309addb6f21b44cbe1b846f3a447d78238f01410479ff722ee4dfd880e307d06fc50a248a9f73a57998a65fd95c48436400280372cf9e99a9952ded7723a68118d4dcf658efbaed2a73265fc63b44789d2d459637ffffffff030000000000000000306a2e6964247c503a2e30a905cb515cfbc291766dfa00000000000000000000000000535441434b5300000000000000647c150000000000001976a914f3c49407d41b82f30636f5180718bb658ce7fe9488ac2c3a0300000000001976a914e1762290e3f035ea4e7f8cbf72a9d9386c4020ab88ac000000000100000001f433cac39fa99d6621e10148fdba962a98c0647214fb6a050c742cc423528cbb010000006a473044022043cbd8b3a1a7f3eb6add66c3af952d27556afc700c48719792ea16c0b553b7ca02204c9c7254d64a58f3bf8b1820bcb97a177ced97f9458fbe3e1039110560d2da7e012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff0220a10700000000001976a914f3c49407d41b82f30636f5180718bb658ce7fe9488ac10661429010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac0000000001000000012ee074f816ebb800d5b3e8497498a8be0b7a578d831b7a4617090f224d20387c010000006a473044022037a59ae75fd04216cf466b49aa22b2f13b9138009d2a399199a1add8dcedc102022032d50e8fe0b0004dd191686fe4e7b15e523a1816616ef9cf1362059941d8cdfd012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff0220a10700000000001976a914f3c49407d41b82f30636f5180718bb658ce7fe9488ac74af0c29010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac000000000100000001b4963d5c40a849f865a884e68a837d7629cfbdca449f53131ee1f54c8517e3a8000000008a473044022057c1bb8264ea497db03388d6ffd7db0e0e9649b9c26c38b0fac52384b8d0582102201520db2bc33b3dfa850e2d53f82495cdbfb4fc48f7fa3f30182ed17b397effd5014104447019ded953edd1bcecffbc66a555f822675257bacc0d357c1dc5194849367354c551e2c2e2048cb927985c8528e24120addd9aa0a2c68b23b462f337caaebcffffffff030000000000000000306a2e6964247c503a2e30a905cb515cfbc291766dfa00000000000000000000000000535441434b5300000000000000647c150000000000001976a914afc75a8f8fbcb922248a663dec927b33dccaed3788ac2c3a0300000000001976a914f3c49407d41b82f30636f5180718bb658ce7fe9488ac000000000100000001b4963d5c40a849f865a884e68a837d7629cfbdca449f53131ee1f54c8517e3a8010000006a47304402207d0349a5ef65a42694fedc2baff5baab8154466c8e8942787c6b1476fddbbba902204f6147a332adaa1313195e6a2ca9e545d00d233e5365ff4e7476c6b69a1808bb012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff0220a10700000000001976a914afc75a8f8fbcb922248a663dec927b33dccaed3788acd8f80429010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac0000000001000000011420e72c7cf93746e3a51c79798dfc3d92efcc5c035bfb6e25c573b34651fd2d010000006a473044022011d7c5e4326e3f1d469c93795627473b78a7db9fd2a0ef5a89c1640a7cd35c9b0220573811fb6831fa031ea5396c39e3a8bf72ec90accbcc22e63e07c3c197894eeb012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff0220a10700000000001976a914afc75a8f8fbcb922248a663dec927b33dccaed3788ac3c42fd28010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac00000000010000000104c89617c9100361301adc113cc8420f0a2884465879612e2c3e7702c18e8bbe000000008b483045022100c83dc003151b3dfec89c1d6a51be38f226754c57139fbdd01bbd73db06fece19022047a130601c35db08603c0e28c8ad0bee6f69b881a61cf5285ffd816d09d53889014104a96a8355b6c3597bb9425c2ef264ab8179ca8acd3032b62980d2067261b37666b66510983e6d60d49bbd28129f0bae4dbcaa97c2bc61a6b2e48ca1625ce81335ffffffff030000000000000000306a2e6964247c503a2e30a905cb515cfbc291766dfa00000000000000000000000000535441434b5300000000000000647c150000000000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac2c3a0300000000001976a914afc75a8f8fbcb922248a663dec927b33dccaed3788ac00000000010000000104c89617c9100361301adc113cc8420f0a2884465879612e2c3e7702c18e8bbe010000006a4730440220577b2b6b1fd42425e6cb6d10076c3312253c0ba4d511a253d252474fb4f13e3f022013b0404d190e7d1b13c3d9003425ac80dc5b0f80375cbc3a71ebff849fed4b63012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff0220a10700000000001976a91474178497e927ff3ff1428a241be454d393c3c91c88aca08bf528010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac000000000100000003738deab0751512cd7c580f748a56e801a88e4b929efbe1944a51304f0f416989000000006a47304402205675beb7b57e0b97a8688dedd291a553fa189d018e4ce4b781aed9736578c4f402207849244e504863fb80904da0de12a76a409c827cc70af8a537389f7c11299ec6012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff738deab0751512cd7c580f748a56e801a88e4b929efbe1944a51304f0f416989010000006b4830450221008bb5d50aac7becf4a6b2207778c2061e54bc99648d9afb28bde4f6789d417f1002202359a08ec2bc1f56807f4c25996fe4d0f282b9e266631b6026f544620259d9f7012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffffe863dfc5fbf69280d9f9c93d861440d2e1eee329eccf5545213f73a809560378010000006a473044022037c9f5df920dbf550c7cc92ad1aa1627851bb9af492563716bd0da060cecb5590220715b5d0b73e108fdc6264e67fb56a77d409e6ec185b93de5219695368ee4292f012103d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3feffffff01c02cfd28010000001976a91474178497e927ff3ff1428a241be454d393c3c91c88ac0000000001000000031420e72c7cf93746e3a51c79798dfc3d92efcc5c035bfb6e25c573b34651fd2d000000008a47304402202c963a8bd001257aefc0853e0dfda571dcde127be9b7a1221c352776b411032e022009d154b93ee42d8c2ca9c24a5cf9e0185d097240c7f7986ea3eb527ca92d78f2014104a96a8355b6c3597bb9425c2ef264ab8179ca8acd3032b62980d2067261b37666b66510983e6d60d49bbd28129f0bae4dbcaa97c2bc61a6b2e48ca1625ce81335feffffffbdb21912e44ba0d0219b15fb1ba735ea2c698930a13575a8db352a48dbd1fe12010000008a47304402201e084a2b11eed752ac476f31f92a979936f7f2915d644bd9485dc06fa1b80d9e02202b3e288ff9501938882ed932280f06204eead8aa41b8ecb5c4d3eecbe25192c6014104a96a8355b6c3597bb9425c2ef264ab8179ca8acd3032b62980d2067261b37666b66510983e6d60d49bbd28129f0bae4dbcaa97c2bc61a6b2e48ca1625ce81335feffffffc1918a15bce400ef116e57af1142857ce2e652d73314628d07cbec67793599c1000000008b483045022100eeebccac3625b93f9c1b404599b02f57868a62b49184c0818125fcff6775ca9f0220768cae6e58346127a85e38ba031a2f7732d1c051d9afed0d3a6c53fcc9d15b55014104a96a8355b6c3597bb9425c2ef264ab8179ca8acd3032b62980d2067261b37666b66510983e6d60d49bbd28129f0bae4dbcaa97c2bc61a6b2e48ca1625ce81335feffffff0190f22700000000001976a914afc75a8f8fbcb922248a663dec927b33dccaed3788ac0000000001000000041c18a687e19c484387daff4b136d2e35f8a4ff74e9901a985f422316e0d33789020000008a47304402202c36873b52b4c042326562fc6442b94f03078ac62cb15c79849fd62ac4abb2a20220022333118b124beda77fa54e07d785d57f2603f9517d4980cf26d4f4ce680140014104ef29f16c10aa2d0468d7841cfedb8b5729689ebca4db38fb8f3fc9ab158e799b6d6dfc2bca52fe490f7acd38e351bf1d28b8f1f48736a0b022f806dd107a8385feffffff28877bb55365102a106d3150dec58e23c4e38fa6d19f6cedd6d4e3bb4dc5f213010000008a47304402204f6cab3fce36ea750538ffe165713bdedc44e8ea5089420634954f9efb8022ff022006c2812283b40fd6e8f26be8ac7261333b90b76c1722df1f9485e3e1e0795053014104ef29f16c10aa2d0468d7841cfedb8b5729689ebca4db38fb8f3fc9ab158e799b6d6dfc2bca52fe490f7acd38e351bf1d28b8f1f48736a0b022f806dd107a8385feffffffc517ff49a374f8a41dd7a5d4028315374f875bd483a4e56bf946d76a0ec441f7000000008b483045022100b74cfc519a6ce6510143072b5bd879fac7eed1415d0df52620a54a40f17470fa022072ffaa0216a9d57d59714e0f01c173097bf9d9558cbdcc77d19152ed8a328932014104ef29f16c10aa2d0468d7841cfedb8b5729689ebca4db38fb8f3fc9ab158e799b6d6dfc2bca52fe490f7acd38e351bf1d28b8f1f48736a0b022f806dd107a8385feffffffe77c28db26558695fbc66172878412cb2694db40ff360b55d3740a44ba2b3238000000008a47304402203e427cc1990a299295554579f30974f83ba05ddf187482140928018e4938d96602204799d8c12cb696eacfd951198d35f0c6a65ecb67b717f55a20afa6677f4bafc4014104ef29f16c10aa2d0468d7841cfedb8b5729689ebca4db38fb8f3fc9ab158e799b6d6dfc2bca52fe490f7acd38e351bf1d28b8f1f48736a0b022f806dd107a8385feffffff0104332800000000001976a91441a349571d89decfac52ffecd92300b6a97b284188ac0000000001000000042ee074f816ebb800d5b3e8497498a8be0b7a578d831b7a4617090f224d20387c000000008a47304402207d9b6519328111fb1ccbf64632a404bd000fbd4e0086204f4853297ddc119fb202206e2b52c5a5c96376a0f95dba0bd086e6bade504c13af7a79aba33e1bd3024193014104447019ded953edd1bcecffbc66a555f822675257bacc0d357c1dc5194849367354c551e2c2e2048cb927985c8528e24120addd9aa0a2c68b23b462f337caaebcfeffffff58ac7561601d65f73a329c291970d755da1923458a8197de4dc9d47061fe5cc4020000008a47304402205c742445dac4de43bc5568b8f00e855fdc454b33ecae730c4be8d01a4479e7ba022061d0d274ba1fdecff47a9d0e1614d710c0e746ab22b5d0bd0ae4fab921e64e99014104447019ded953edd1bcecffbc66a555f822675257bacc0d357c1dc5194849367354c551e2c2e2048cb927985c8528e24120addd9aa0a2c68b23b462f337caaebcfeffffff6c1828ac45acb5040e9a8eb3f228942d562bde81923158da6425276b27e62155000000008b483045022100cb803162888cb25f25c2f1fa7340b27ffc50c2aba869e9bddcb97b43c3feed470220501807396b9d48508cf31efaa212358615b241493134811dac43e6a372987c51014104447019ded953edd1bcecffbc66a555f822675257bacc0d357c1dc5194849367354c551e2c2e2048cb927985c8528e24120addd9aa0a2c68b23b462f337caaebcfeffffffb4a2a1a056cddf86c811ab41f4b2dfd8f29feec73d34c796ea28cf12b8f81cae010000008b483045022100d0711f0295c01fe8856e8e38b37fd922b8aa0352b6d13616c621fa0f7d4b5ff1022059aafb91eea435e797d0d27477a63f06454c470a0d8ef8e5e8d496fb62c1efd3014104447019ded953edd1bcecffbc66a555f822675257bacc0d357c1dc5194849367354c551e2c2e2048cb927985c8528e24120addd9aa0a2c68b23b462f337caaebcfeffffff0104332800000000001976a914f3c49407d41b82f30636f5180718bb658ce7fe9488ac000000000100000004910223cea04cb252e6e6699bb38e77e63336680f1da01d35ebda1786ae607c7c010000008b483045022100961a28548fc3e53962e1b4039b383b0cf441c3ff76433e7652840c5cc8f711fd022074f177613090d1d1d8149eaa71f1f72531c4b0b8136be4bdb852e0ebf94fdeda01410479ff722ee4dfd880e307d06fc50a248a9f73a57998a65fd95c48436400280372cf9e99a9952ded7723a68118d4dcf658efbaed2a73265fc63b44789d2d459637feffffff9e3b6d11ace91dab509d257da4f54a637a38f188b13f8fe4a3a5b0fce6af2ac5020000008b483045022100a0c9d78f7b3db154d8c9c437b087be2e8f1adb01c40148307e8c986c0855192402201f9cfa8a422f39125cd5e90b0128558f30daa6e339da95d93673ed8331bdceab01410479ff722ee4dfd880e307d06fc50a248a9f73a57998a65fd95c48436400280372cf9e99a9952ded7723a68118d4dcf658efbaed2a73265fc63b44789d2d459637feffffffca544deb6c248c68a56d86e3e9fa2f93fcf35d6055acb116962421eb4041e896000000008a47304402202274c0b8b6634494b65309d4e2a0a91d6a55051000485c427d1c05d5cca810f002204f5eeb5c202b2e7c4ef1b1f61e3a9928fa8b2014f09a2735305ec1a2a1cb2dd501410479ff722ee4dfd880e307d06fc50a248a9f73a57998a65fd95c48436400280372cf9e99a9952ded7723a68118d4dcf658efbaed2a73265fc63b44789d2d459637feffffffd3600cc4caa4d7719a1f7b78f8d66e09cd84a6e4e7ee4cceff44921a314502ec000000008b4830450221008b7a7d15efa590a750ac917cad343abbaf432858de0df37bd3d0e660bcd515800220360cb45eb1a77cb4ad90d595f91f4018aa5662e36add18b7131ea99b3dd9071101410479ff722ee4dfd880e307d06fc50a248a9f73a57998a65fd95c48436400280372cf9e99a9952ded7723a68118d4dcf658efbaed2a73265fc63b44789d2d459637feffffff0104332800000000001976a914e1762290e3f035ea4e7f8cbf72a9d9386c4020ab88ac00000000".to_owned(),
                header: "00000020ad98a2888b7c69f4187ef5ee1b5921a6fb62803aa8bd35826f7fb751714baf250cb5ef03478d35ed7f6582ab40232ee39744471b2bcb40b91db0f29102d695123379fc5bffff7f2002000000".to_owned(),
                height: 32,
                result: Some(BitcoinBlock {
                    block_height: 32,
                    block_hash: to_block_hash(&hex_bytes("4f3757bc236e58b87d6208aa795115002b739bf39268cf69640f0b092e8cdafe").unwrap()),
                    parent_block_hash: to_block_hash(&hex_bytes("25af4b7151b77f6f8235bda83a8062fba621591beef57e18f4697c8b88a298ad").unwrap()),
                    timestamp: 1543272755,
                    txs: vec![
                        BitcoinTransaction {
                            data_amt: 0,
                            // TOKEN_TRANSFER
                            txid: to_txid(&hex_bytes("13f2c54dbbe3d4d6ed6c9fd1a68fe3c4238ec5de50316d102a106553b57b8728").unwrap()),
                            vtxindex: 2,
                            opcode: b'$',
                            data: hex_bytes("7c503a2e30a905cb515cfbc291766dfa00000000000000000000000000535441434b530000000000000064").unwrap(),
                            inputs: vec![
                                BitcoinTxInputStructured {
                                    keys: vec![
                                        BitcoinPublicKey::from_hex("03d6fd1ba0effaf1e8d94ea7b7a3d0ef26fea00a14ce5ffcc1495fe588a2c6d0f3").unwrap()
                                    ],
                                    num_required: 1,
                                    in_type: BitcoinInputType::Standard,
                                    tx_ref: (Txid::from_hex("f741c40e6ad746f96be5a483d45b874f37158302d4a5d71da4f874a349ff17c5").unwrap(), 1),
                                }.into()
                            ],
                            outputs: vec![
                                BitcoinTxOutput {
                                    units: 5500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("41a349571d89decfac52ffecd92300b6a97b2841").unwrap()).unwrap()
                                },
                                BitcoinTxOutput {
                                    units: 4986192000,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("74178497e927ff3ff1428a241be454d393c3c91c").unwrap()).unwrap()
                                }
                            ]
                        },
                        BitcoinTransaction {
                            data_amt: 0,
                            // TOKEN_TRANSFER 
                            txid: to_txid(&hex_bytes("7c7c60ae8617daeb351da01d0f683633e6778eb39b69e6e652b24ca0ce230291").unwrap()),
                            vtxindex: 4,
                            opcode: b'$',
                            data: hex_bytes("7c503a2e30a905cb515cfbc291766dfa00000000000000000000000000535441434b530000000000000064").unwrap(),
                            inputs: vec![
                                BitcoinTxInputStructured {
                                    keys: vec![
                                        BitcoinPublicKey::from_hex("04ef29f16c10aa2d0468d7841cfedb8b5729689ebca4db38fb8f3fc9ab158e799b6d6dfc2bca52fe490f7acd38e351bf1d28b8f1f48736a0b022f806dd107a8385").unwrap()
                                    ],
                                    num_required: 1,
                                    in_type: BitcoinInputType::Standard,
                                    tx_ref: (Txid::from_hex("ba53c1d5b3d18115d7a9f7402e7c96281a05af1835f98dc8d729158c96d31193").unwrap(), 0),
                                }.into()
                            ],
                            outputs: vec![
                                BitcoinTxOutput {
                                    units: 5500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("e1762290e3f035ea4e7f8cbf72a9d9386c4020ab").unwrap()).unwrap()
                                },
                                BitcoinTxOutput {
                                    units: 211500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("41a349571d89decfac52ffecd92300b6a97b2841").unwrap()).unwrap()
                                }
                            ]
                        },
                        BitcoinTransaction {
                            data_amt: 0,
                            // TOKEN_TRANSFER 
                            txid: to_txid(&hex_bytes("ae1cf8b812cf28ea96c7343dc7ee9ff2d8dfb2f441ab11c886dfcd56a0a1a2b4").unwrap()),
                            vtxindex: 7,
                            opcode: b'$',
                            data: hex_bytes("7c503a2e30a905cb515cfbc291766dfa00000000000000000000000000535441434b530000000000000064").unwrap(),
                            inputs: vec![
                                BitcoinTxInputStructured {
                                    keys: vec![
                                        BitcoinPublicKey::from_hex("0479ff722ee4dfd880e307d06fc50a248a9f73a57998a65fd95c48436400280372cf9e99a9952ded7723a68118d4dcf658efbaed2a73265fc63b44789d2d459637").unwrap()
                                    ],
                                    num_required: 1,
                                    in_type: BitcoinInputType::Standard,
                                    tx_ref: (Txid::from_hex("bb8c5223c42c740c056afb147264c0982a96bafd4801e121669da99fc3ca33f4").unwrap(), 0),
                                }.into()
                            ],
                            outputs: vec![
                                BitcoinTxOutput {
                                    units: 5500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("f3c49407d41b82f30636f5180718bb658ce7fe94").unwrap()).unwrap()
                                },
                                BitcoinTxOutput {
                                    units: 211500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("e1762290e3f035ea4e7f8cbf72a9d9386c4020ab").unwrap()).unwrap()
                                }
                            ]
                        },
                        BitcoinTransaction {
                            data_amt: 0,
                            // TOKEN_TRANSFER
                            txid: to_txid(&hex_bytes("12fed1db482a35dba87535a13089692cea35a71bfb159b21d0a04be41219b2bd").unwrap()),
                            vtxindex: 10,
                            opcode: b'$',
                            data: hex_bytes("7c503a2e30a905cb515cfbc291766dfa00000000000000000000000000535441434b530000000000000064").unwrap(),
                            inputs: vec![
                                BitcoinTxInputStructured {
                                    keys: vec![
                                        BitcoinPublicKey::from_hex("04447019ded953edd1bcecffbc66a555f822675257bacc0d357c1dc5194849367354c551e2c2e2048cb927985c8528e24120addd9aa0a2c68b23b462f337caaebc").unwrap()
                                    ],
                                    num_required: 1,
                                    in_type: BitcoinInputType::Standard,
                                    tx_ref: (Txid::from_hex("a8e317854cf5e11e13539f44cabdcf29767d838ae684a865f849a8405c3d96b4").unwrap(), 0),
                                }.into()
                            ],
                            outputs: vec![
                                BitcoinTxOutput {
                                    units: 5500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("afc75a8f8fbcb922248a663dec927b33dccaed37").unwrap()).unwrap()
                                },
                                BitcoinTxOutput {
                                    units: 211500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("f3c49407d41b82f30636f5180718bb658ce7fe94").unwrap()).unwrap()
                                }
                            ]
                        },
                        BitcoinTransaction {
                            data_amt: 0,
                            // TOKEN_TRANSFER 
                            txid: to_txid(&hex_bytes("78035609a8733f214555cfec29e3eee1d24014863dc9f9d98092f6fbc5df63e8").unwrap()),
                            vtxindex: 13,
                            opcode: b'$',
                            data: hex_bytes("7c503a2e30a905cb515cfbc291766dfa00000000000000000000000000535441434b530000000000000064").unwrap(),
                            inputs: vec![
                                BitcoinTxInputStructured {
                                    keys: vec![
                                        BitcoinPublicKey::from_hex("04a96a8355b6c3597bb9425c2ef264ab8179ca8acd3032b62980d2067261b37666b66510983e6d60d49bbd28129f0bae4dbcaa97c2bc61a6b2e48ca1625ce81335").unwrap()
                                    ],
                                    num_required: 1,
                                    in_type: BitcoinInputType::Standard,
                                    tx_ref: (Txid::from_hex("be8b8ec102773e2c2e6179584684280a0f42c83c11dc1a30610310c91796c804").unwrap(), 0),
                                }.into()
                            ],
                            outputs: vec![
                                BitcoinTxOutput {
                                    units: 5500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("74178497e927ff3ff1428a241be454d393c3c91c").unwrap()).unwrap()
                                },
                                BitcoinTxOutput {
                                    units: 211500,
                                    address: BitcoinAddress::from_bytes_legacy(BitcoinNetworkType::Testnet, LegacyBitcoinAddressType::PublicKeyHash, &hex_bytes("afc75a8f8fbcb922248a663dec927b33dccaed37").unwrap()).unwrap()
                                }
                            ]
                        }
                    ]
                })
            },
            BlockFixture {
                // invalid data -- merkle root won't match transactions (so header won't match)
                block: "000000209cef4ccd19f4294dd5c762aab6d9577fb4412cd4c0a662a953a8b7969697bc1ddab52e6f053758022fb92f04388eb5fdd87046776e9c406880e728b48e6930aff462fc5bffff7f200000000002020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff0502b5020101ffffffff024018a41200000000232103f51f0c868fd99a4a3a14fe2153fba3c5f635c31bf0a588545627134b49609097ac0000000000000000266a24aa21a9ed18a09ae86261d6802bff7fa705afa558764ed3750c2273bfae5b5136c44d14d6012000000000000000000000000000000000000000000000000000000000000000000000000001000000000101a7ef2b09722ad786c569c0812005a731ce19290bb0a2afc16cb91056c2e4c19e0100000017160014393ffec4f09b38895b8502377693f23c6ae00f19ffffffff0300000000000000000d6a0b69643a666f6f2e746573747c1500000000000017a9144b85301ba8e42bf98472b8ed4939d5f76b98fcea87144d9c290100000017a91431f8968eb1730c83fb58409a9a560a0a0835027f8702483045022100fc82815edf1c0ef0c601cf1e26494626d7b01597be5ab83df025ff1ee67730130220016c4c29d77aadb5ff57c0c9272a43950ca29b84d8adfaed95ac69db90b35d5b012102d341f728783eb93e6fb5921a1ebe9d149e941de31e403cd69afa2f0f1e698e8100000000".to_owned(),
                header: "000000209cef4ccd19f4294dd5c762aab6d9577fb4412cd4c0a662a953a8b7969697bc1ddab52e6f053758022fb92f04388eb5fdd87046776e9c406880e728b48e6931aff462fc5bffff7f2000000000".to_owned(),
                height: 32,
                result: None,
            }
        ];

        let parser = BitcoinBlockParser::new(BitcoinNetworkType::Testnet, MagicBytes([105, 100])); // "id"
        for block_fixture in block_fixtures {
            let block = make_block(&block_fixture.block).unwrap();
            let header = make_block_header(&block_fixture.header).unwrap();
            let height = block_fixture.height;

            let parsed_block_opt =
                parser.process_block(&block, &header, height, StacksEpochId::Epoch2_05);
            assert_eq!(parsed_block_opt, block_fixture.result);
        }
    }
}
