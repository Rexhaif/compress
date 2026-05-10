use crate::error::{Error, Result};

const BIT_MODEL_TOTAL: u32 = 1 << 11;
const MOVE_BITS: u32 = 5;
const RC_BIT_PRICE_SHIFT_BITS: u32 = 4;
const RC_INFINITY_PRICE: u32 = 1 << 30;
const RC_MOVE_REDUCING_BITS: u32 = 4;
const RC_PRICE_TABLE_SIZE: usize = (BIT_MODEL_TOTAL >> RC_MOVE_REDUCING_BITS) as usize;
const NUM_ALIGN_BITS: u32 = 4;
const NUM_FULL_DISTANCES: usize = 128;
const NUM_LEN_TO_POS_STATES: usize = 4;
const NUM_POS_BITS_MAX: usize = 4;
const NUM_POS_SLOT_BITS: u32 = 6;
const NUM_POS_STATES_MAX: usize = 1 << NUM_POS_BITS_MAX;
const NUM_STATES: usize = 12;
const LONG_MATCH_FAST_PATH_MIN: u32 = 4;
const OPTS: usize = 1 << 12;
const OPTIMAL_WINDOW_MAX: usize = 192;
const OPTIMAL_WINDOW_MULTIPLIER: usize = 2;
const START_POS_MODEL_INDEX: u32 = 4;
const END_POS_MODEL_INDEX: u32 = 14;
const MATCH_LEN_MAX: usize = 273;
const MATCH_LEN_MIN: usize = 2;
const LEN_SYMBOLS: usize = MATCH_LEN_MAX - MATCH_LEN_MIN + 1;
const MATCH_PRICE_REFRESH: u32 = 1 << 7;
const EMPTY_MATCH: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompressionMode {
    Fast,
    Normal,
    Optimal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchFinderKind {
    Bt4,
}

#[derive(Clone, Copy, Debug)]
pub struct EncoderOptions {
    pub depth: u32,
    pub dict_size: u32,
    pub match_finder: MatchFinderKind,
    pub mode: CompressionMode,
    pub nice: u32,
    pub properties: LzmaProperties,
}

#[derive(Clone, Copy, Debug)]
pub struct LzmaProperties {
    pub lc: u32,
    pub lp: u32,
    pub pb: u32,
}

impl LzmaProperties {
    pub fn decode(byte: u8) -> Result<LzmaProperties> {
        if byte >= 9 * 5 * 5 {
            return Err(Error::Format("invalid LZMA properties"));
        }

        let lc = u32::from(byte % 9);
        let remainder = u32::from(byte / 9);
        let lp = remainder % 5;
        let pb = remainder / 5;

        Ok(LzmaProperties { lc, lp, pb })
    }

    pub fn encode(self) -> Result<u8> {
        if self.lc > 4 {
            return Err(Error::Usage("lc must be <= 4"));
        }

        if self.lp > 4 {
            return Err(Error::Usage("lp must be <= 4"));
        }

        if self.pb > 4 {
            return Err(Error::Usage("pb must be <= 4"));
        }

        if self.lc + self.lp > 4 {
            return Err(Error::Usage("lc + lp must be <= 4"));
        }

        Ok(((self.pb * 5 + self.lp) * 9 + self.lc) as u8)
    }
}

pub struct LzmaEncoder {
    dictionary_start: usize,
    dict_size: u32,
    align_price_count: u32,
    align_prices: [u32; 1 << NUM_ALIGN_BITS],
    dist_prices: Vec<u32>,
    dist_slot_prices: Vec<u32>,
    dist_table_size: usize,
    finder: MatchFinderBt4,
    is_match: Vec<u16>,
    is_rep: [u16; NUM_STATES],
    is_rep_g0: [u16; NUM_STATES],
    is_rep_g1: [u16; NUM_STATES],
    is_rep_g2: [u16; NUM_STATES],
    is_rep0_long: Vec<u16>,
    len_encoder: LenDecoder,
    literal: Vec<u16>,
    match_price_count: u32,
    mode: CompressionMode,
    nice: usize,
    opts: Vec<OptimalNode>,
    pending_decisions: Vec<ParseDecision>,
    pending_index: usize,
    pos_align: [u16; 1 << NUM_ALIGN_BITS],
    pos_decoders: [u16; NUM_FULL_DISTANCES],
    pos_slot: Vec<u16>,
    properties: LzmaProperties,
    rep_len_encoder: LenDecoder,
    reps: [u32; 4],
    state: u32,
}

impl LzmaEncoder {
    pub fn new(options: EncoderOptions, input_len: usize) -> LzmaEncoder {
        debug_assert_eq!(options.match_finder, MatchFinderKind::Bt4);

        let literal_contexts = 1usize << (options.properties.lc + options.properties.lp);
        let nice = options
            .nice
            .clamp(MATCH_LEN_MIN as u32, MATCH_LEN_MAX as u32) as usize;
        let mut encoder = LzmaEncoder {
            dictionary_start: 0,
            dict_size: options.dict_size,
            align_price_count: u32::MAX / 2,
            align_prices: [0; 1 << NUM_ALIGN_BITS],
            dist_prices: vec![0; NUM_LEN_TO_POS_STATES * NUM_FULL_DISTANCES],
            dist_slot_prices: vec![0; NUM_LEN_TO_POS_STATES * (1 << NUM_POS_SLOT_BITS)],
            dist_table_size: dist_table_size(options.dict_size),
            finder: MatchFinderBt4::new(
                input_len,
                options.depth,
                options.mode,
                options.dict_size,
                nice,
            ),
            is_match: vec![0; NUM_STATES * NUM_POS_STATES_MAX],
            is_rep: [0; NUM_STATES],
            is_rep_g0: [0; NUM_STATES],
            is_rep_g1: [0; NUM_STATES],
            is_rep_g2: [0; NUM_STATES],
            is_rep0_long: vec![0; NUM_STATES * NUM_POS_STATES_MAX],
            len_encoder: LenDecoder::new(),
            literal: vec![0; literal_contexts * 0x300],
            match_price_count: u32::MAX / 2,
            mode: options.mode,
            nice,
            opts: vec![OptimalNode::empty(); OPTS],
            pending_decisions: Vec::with_capacity(OPTS),
            pending_index: 0,
            pos_align: [0; 1 << NUM_ALIGN_BITS],
            pos_decoders: [0; NUM_FULL_DISTANCES],
            pos_slot: vec![0; NUM_LEN_TO_POS_STATES * (1 << NUM_POS_SLOT_BITS)],
            properties: options.properties,
            rep_len_encoder: LenDecoder::new(),
            reps: [0; 4],
            state: 0,
        };

        encoder.len_encoder.set_table_size(nice + 1 - MATCH_LEN_MIN);
        encoder
            .rep_len_encoder
            .set_table_size(nice + 1 - MATCH_LEN_MIN);
        encoder.reset_state();
        encoder
    }

    pub fn reset_state(&mut self) {
        fill_probs(&mut self.is_match);
        fill_probs(&mut self.is_rep);
        fill_probs(&mut self.is_rep_g0);
        fill_probs(&mut self.is_rep_g1);
        fill_probs(&mut self.is_rep_g2);
        fill_probs(&mut self.is_rep0_long);
        fill_probs(&mut self.literal);
        fill_probs(&mut self.pos_align);
        fill_probs(&mut self.pos_decoders);
        fill_probs(&mut self.pos_slot);

        self.len_encoder.reset();
        self.rep_len_encoder.reset();
        self.reps = [0; 4];
        self.state = 0;
        self.pending_decisions.clear();
        self.pending_index = 0;
        self.match_price_count = u32::MAX / 2;
        self.align_price_count = u32::MAX / 2;
        self.refresh_price_tables();
    }

    pub fn reset_dictionary(&mut self, options: EncoderOptions, input_len: usize) {
        debug_assert_eq!(options.match_finder, MatchFinderKind::Bt4);

        self.dictionary_start = 0;
        self.finder = MatchFinderBt4::new(
            input_len,
            options.depth,
            options.mode,
            options.dict_size,
            self.nice,
        );
        self.reset_state();
    }

    pub(crate) fn snapshot_state(&self) -> LzmaEncoderState {
        LzmaEncoderState {
            align_price_count: self.align_price_count,
            align_prices: self.align_prices,
            dist_prices: self.dist_prices.clone(),
            dist_slot_prices: self.dist_slot_prices.clone(),
            is_match: self.is_match.clone(),
            is_rep: self.is_rep,
            is_rep_g0: self.is_rep_g0,
            is_rep_g1: self.is_rep_g1,
            is_rep_g2: self.is_rep_g2,
            is_rep0_long: self.is_rep0_long.clone(),
            len_encoder: self.len_encoder.clone(),
            literal: self.literal.clone(),
            match_price_count: self.match_price_count,
            pos_align: self.pos_align,
            pos_decoders: self.pos_decoders,
            pos_slot: self.pos_slot.clone(),
            rep_len_encoder: self.rep_len_encoder.clone(),
            reps: self.reps,
            state: self.state,
        }
    }

    pub(crate) fn restore_state(&mut self, state: LzmaEncoderState) {
        self.align_price_count = state.align_price_count;
        self.align_prices = state.align_prices;
        self.dist_prices = state.dist_prices;
        self.dist_slot_prices = state.dist_slot_prices;
        self.is_match = state.is_match;
        self.is_rep = state.is_rep;
        self.is_rep_g0 = state.is_rep_g0;
        self.is_rep_g1 = state.is_rep_g1;
        self.is_rep_g2 = state.is_rep_g2;
        self.is_rep0_long = state.is_rep0_long;
        self.len_encoder = state.len_encoder;
        self.literal = state.literal;
        self.match_price_count = state.match_price_count;
        self.pos_align = state.pos_align;
        self.pos_decoders = state.pos_decoders;
        self.pos_slot = state.pos_slot;
        self.rep_len_encoder = state.rep_len_encoder;
        self.reps = state.reps;
        self.state = state.state;
        self.pending_decisions.clear();
        self.pending_index = 0;
    }

    pub(crate) fn encode_range_limited(
        &mut self,
        input: &[u8],
        start: usize,
        end: usize,
        dictionary_start: usize,
        output_limit: usize,
    ) -> Result<Option<Vec<u8>>> {
        self.encode_range_inner(input, start, end, dictionary_start, Some(output_limit))
    }

    pub(crate) fn observe_uncompressed_range(
        &mut self,
        input: &[u8],
        start: usize,
        end: usize,
        dictionary_start: usize,
    ) {
        self.dictionary_start = dictionary_start;
        self.pending_decisions.clear();
        self.pending_index = 0;

        for position in start..end {
            self.finder.insert(input, position);
        }
    }

    fn encode_range_inner(
        &mut self,
        input: &[u8],
        start: usize,
        end: usize,
        dictionary_start: usize,
        output_limit: Option<usize>,
    ) -> Result<Option<Vec<u8>>> {
        let mut range = RangeEncoder::new(output_limit);
        let mut position = start;
        self.dictionary_start = dictionary_start;

        while position < end {
            let decision = self.parse_position(input, position, end);
            self.encode_decision(&mut range, input, position, decision)?;
            if range.output_limit_reached() {
                return Ok(None);
            }

            position += decision.length as usize;
        }

        Ok(range.finish())
    }

    fn parse_position(&mut self, input: &[u8], position: usize, end: usize) -> ParseDecision {
        if let Some(decision) = self.next_pending_decision() {
            return decision;
        }

        self.refresh_price_tables();

        if position == self.dictionary_start {
            self.finder.insert(input, position);
            return ParseDecision::literal();
        }

        match self.mode {
            CompressionMode::Fast => self.parse_greedy_position(input, position, end, false),
            CompressionMode::Normal => self.parse_greedy_position(input, position, end, true),
            CompressionMode::Optimal => self.parse_optimal_position(input, position, end),
        }
    }

    fn next_pending_decision(&mut self) -> Option<ParseDecision> {
        if self.pending_index >= self.pending_decisions.len() {
            return None;
        }

        let decision = self.pending_decisions[self.pending_index];
        self.pending_index += 1;

        if self.pending_index == self.pending_decisions.len() {
            self.pending_decisions.clear();
            self.pending_index = 0;
        }

        Some(decision)
    }

    fn parse_greedy_position(
        &mut self,
        input: &[u8],
        position: usize,
        end: usize,
        lazy: bool,
    ) -> ParseDecision {
        let mut matches = MatchList::new();
        self.finder.find_matches(
            input,
            position,
            end,
            self.dict_size as usize,
            self.nice,
            &mut matches,
        );

        let reps = self.rep_matches(input, position, end);
        let mut decision = choose_decision(adjusted_normal_candidate(&matches), &reps);
        if lazy {
            decision = self.lazy_greedy_decision(input, position, end, decision);
        }

        self.finder
            .skip_insert(input, position + 1, position + decision.length as usize);

        decision
    }

    fn lazy_greedy_decision(
        &self,
        input: &[u8],
        position: usize,
        end: usize,
        decision: ParseDecision,
    ) -> ParseDecision {
        if decision.kind == DecisionKind::Literal {
            return decision;
        }

        if decision.length < 3 || position + 1 >= end {
            return decision;
        }

        let mut matches = MatchList::new();
        self.finder.peek_matches(
            input,
            position + 1,
            end,
            self.dict_size as usize,
            self.nice,
            &mut matches,
        );

        let normal = better_normal(
            adjusted_normal_candidate(&matches),
            synthetic_next_match(input, position + 1, position, end, self.nice),
        );
        let reps = self.rep_matches(input, position + 1, end);
        if decision.kind == DecisionKind::Match {
            if let Some(next_normal) = normal
                && lazy_next_normal_beats_current(decision, next_normal)
            {
                return ParseDecision::literal();
            }

            let rep_limit = (decision.length.saturating_sub(1)).max(MATCH_LEN_MIN as u32);
            if reps.iter().any(|rep| rep.length >= rep_limit) {
                return ParseDecision::literal();
            }
        }

        let next = choose_decision(normal, &reps);

        if next.kind != DecisionKind::Literal && next.length > decision.length + 1 {
            ParseDecision::literal()
        } else {
            decision
        }
    }

    fn parse_optimal_position(
        &mut self,
        input: &[u8],
        position: usize,
        end: usize,
    ) -> ParseDecision {
        let parse_limit = self.optimal_parse_limit(end - position);
        if parse_limit == 1 {
            self.finder.insert(input, position);
            return ParseDecision::literal();
        }

        let mut current_matches = MatchList::new();
        self.finder.find_matches(
            input,
            position,
            end,
            self.dict_size as usize,
            self.nice,
            &mut current_matches,
        );

        if let Some(decision) = self.fast_path_decision(input, position, end, &current_matches) {
            self.finder
                .skip_insert(input, position + 1, position + decision.length as usize);
            return decision;
        }

        self.prepare_optimal_nodes(parse_limit);
        let mut len_end =
            self.price_current_node(input, position, end, 0, parse_limit, &current_matches);
        len_end = len_end.max(1).min(parse_limit);

        let mut current = 1usize;
        while current < len_end {
            let mut matches = MatchList::new();
            self.finder.find_matches(
                input,
                position + current,
                end,
                self.dict_size as usize,
                self.nice,
                &mut matches,
            );

            let reached =
                self.price_current_node(input, position, end, current, parse_limit, &matches);
            len_end = len_end.max(reached.min(parse_limit));
            current += 1;
        }

        self.queue_optimal_path(len_end)
    }

    fn optimal_parse_limit(&self, available: usize) -> usize {
        let target = (self.nice * OPTIMAL_WINDOW_MULTIPLIER).min(OPTIMAL_WINDOW_MAX.max(self.nice));

        available.min(target).min(OPTS - 1)
    }

    fn fast_path_decision(
        &self,
        input: &[u8],
        position: usize,
        end: usize,
        matches: &MatchList,
    ) -> Option<ParseDecision> {
        let reps = self.rep_matches(input, position, end);
        let rep = best_rep(&reps);

        if rep.length as usize >= self.nice {
            return Some(rep_decision(rep, rep.length));
        }

        if let Some(normal) = adjusted_normal_candidate(matches)
            && normal.length as usize >= self.nice
        {
            return Some(ParseDecision {
                distance: normal.distance,
                kind: DecisionKind::Match,
                length: normal.length,
                rep_index: 0,
            });
        }

        let decision = choose_decision(adjusted_normal_candidate(matches), &reps);
        if decision.length >= LONG_MATCH_FAST_PATH_MIN {
            Some(decision)
        } else {
            None
        }
    }

    fn price_current_node(
        &mut self,
        input: &[u8],
        base_position: usize,
        end: usize,
        current: usize,
        parse_limit: usize,
        matches: &MatchList,
    ) -> usize {
        if self.opts[current].price >= RC_INFINITY_PRICE {
            return current;
        }

        self.price_literal_transition(input, base_position, current, parse_limit);
        let rep_reached =
            self.price_rep_transitions(input, base_position, end, current, parse_limit);
        let match_reached =
            self.price_match_transitions(input, end, matches, base_position, current, parse_limit);
        let literal_rep_reached =
            self.price_literal_rep0_transition(input, base_position, end, current, parse_limit);

        rep_reached.max(match_reached).max(literal_rep_reached)
    }

    fn prepare_optimal_nodes(&mut self, parse_limit: usize) {
        for node in &mut self.opts[..=parse_limit] {
            *node = OptimalNode::empty();
        }

        self.opts[0].price = 0;
        self.opts[0].reps = self.reps;
        self.opts[0].state = self.state;
    }

    fn price_literal_transition(
        &mut self,
        input: &[u8],
        base_position: usize,
        current: usize,
        parse_limit: usize,
    ) {
        if current + 1 > parse_limit {
            return;
        }

        let node = self.opts[current];
        let position = base_position + current;
        let pos_state = self.pos_state(position);
        let match_index = self.match_index(node.state, pos_state);
        let price = node.price
            + rc_bit_0_price(self.is_match[match_index])
            + self.literal_price(input, position, node.state, node.reps);

        self.relax_optimal(
            current,
            current + 1,
            price,
            ParseDecision::literal(),
            state_update_literal(node.state),
            node.reps,
        );
    }

    fn price_rep_transitions(
        &mut self,
        input: &[u8],
        base_position: usize,
        end: usize,
        current: usize,
        parse_limit: usize,
    ) -> usize {
        let node = self.opts[current];
        let position = base_position + current;
        let available = parse_limit - current;
        let pos_state = self.pos_state(position);
        let match_index = self.match_index(node.state, pos_state);
        let rep_prefix = node.price
            + rc_bit_1_price(self.is_match[match_index])
            + rc_bit_1_price(self.is_rep[node.state as usize]);
        let mut reached = current;

        for rep_index in 0..4 {
            let rep_length = self.rep_length(input, position, end, node.reps[rep_index], available);
            if rep_index == 0 && rep_length >= 1 {
                self.price_short_rep(current, rep_prefix, node, match_index);
            }

            if rep_length >= MATCH_LEN_MIN {
                reached = reached.max(current + rep_length);
                self.price_long_rep_range(
                    current, rep_prefix, node, pos_state, rep_index, rep_length,
                );
                reached = reached.max(self.price_rep_literal_rep0_transition(
                    input,
                    base_position,
                    end,
                    current,
                    parse_limit,
                    rep_prefix,
                    node,
                    pos_state,
                    rep_index,
                    rep_length,
                ));
            }
        }

        reached
    }

    fn price_short_rep(
        &mut self,
        current: usize,
        rep_prefix: u32,
        node: OptimalNode,
        match_index: usize,
    ) {
        let price = rep_prefix
            + rc_bit_0_price(self.is_rep_g0[node.state as usize])
            + rc_bit_0_price(self.is_rep0_long[match_index]);
        let decision = ParseDecision {
            distance: node.reps[0],
            kind: DecisionKind::Rep,
            length: 1,
            rep_index: 0,
        };

        self.relax_optimal(
            current,
            current + 1,
            price,
            decision,
            state_update_short_rep(node.state),
            node.reps,
        );
    }

    fn price_long_rep_range(
        &mut self,
        current: usize,
        rep_prefix: u32,
        node: OptimalNode,
        pos_state: u32,
        rep_index: usize,
        rep_length: usize,
    ) {
        let pure_price = self.pure_rep_price(rep_index as u32, node.state, pos_state);

        for length in MATCH_LEN_MIN..=rep_length {
            let decision = ParseDecision {
                distance: node.reps[rep_index],
                kind: DecisionKind::Rep,
                length: length as u32,
                rep_index: rep_index as u32,
            };
            let price = rep_prefix
                + pure_price
                + self
                    .rep_len_encoder
                    .price(length as u32, pos_state as usize);
            let (state, reps) = advance_decision_state(node.state, node.reps, decision);

            self.relax_optimal(current, current + length, price, decision, state, reps);
        }
    }

    fn price_match_transitions(
        &mut self,
        input: &[u8],
        end: usize,
        matches: &MatchList,
        base_position: usize,
        current: usize,
        parse_limit: usize,
    ) -> usize {
        let node = self.opts[current];
        let available = parse_limit - current;
        let position = base_position + current;
        let pos_state = self.pos_state(position);
        let match_index = self.match_index(node.state, pos_state);
        let normal_prefix = node.price
            + rc_bit_1_price(self.is_match[match_index])
            + rc_bit_0_price(self.is_rep[node.state as usize]);
        let mut length = MATCH_LEN_MIN;
        let mut reached = current;

        for candidate in matches.iter() {
            let max_length = (candidate.length as usize).min(available);
            reached = reached.max(current + max_length);
            while length <= max_length {
                let decision = ParseDecision {
                    distance: candidate.distance,
                    kind: DecisionKind::Match,
                    length: length as u32,
                    rep_index: 0,
                };
                let price = normal_prefix
                    + self.dist_len_price(candidate.distance - 1, length as u32, pos_state);
                let (state, reps) = advance_decision_state(node.state, node.reps, decision);

                self.relax_optimal(current, current + length, price, decision, state, reps);
                length += 1;
            }

            reached = reached.max(self.price_match_literal_rep0_transition(
                input,
                end,
                base_position,
                current,
                parse_limit,
                normal_prefix,
                node,
                pos_state,
                candidate,
                max_length,
            ));
        }

        reached
    }

    fn price_literal_rep0_transition(
        &mut self,
        input: &[u8],
        base_position: usize,
        end: usize,
        current: usize,
        parse_limit: usize,
    ) -> usize {
        if current + 1 + MATCH_LEN_MIN > parse_limit {
            return current;
        }

        let node = self.opts[current];
        let position = base_position + current;
        let literal_price =
            node.price + self.literal_transition_price(input, position, node.state, node.reps);
        let state_after_literal = state_update_literal(node.state);
        let rep_position = position + 1;
        let rep_length = self.rep0_length_at(
            input,
            rep_position,
            end,
            node.reps[0],
            parse_limit - current - 1,
        );
        if rep_length < MATCH_LEN_MIN {
            return current;
        }

        let pos_state = self.pos_state(rep_position);
        let price = literal_price
            + self.rep0_match_price(state_after_literal, pos_state, rep_length as u32);
        let literal = ParseDecision::literal();
        let rep = ParseDecision {
            distance: node.reps[0],
            kind: DecisionKind::Rep,
            length: rep_length as u32,
            rep_index: 0,
        };
        let (state, reps) = advance_decision_state(node.state, node.reps, literal);
        let (state, reps) = advance_decision_state(state, reps, rep);
        let edge = [literal, rep, ParseDecision::literal()];
        let to = current + 1 + rep_length;

        self.relax_optimal_edge(current, to, price, edge, 2, state, reps);
        to
    }

    #[allow(clippy::too_many_arguments)]
    fn price_rep_literal_rep0_transition(
        &mut self,
        input: &[u8],
        base_position: usize,
        end: usize,
        current: usize,
        parse_limit: usize,
        rep_prefix: u32,
        node: OptimalNode,
        pos_state: u32,
        rep_index: usize,
        rep_length: usize,
    ) -> usize {
        if current + rep_length + 1 + MATCH_LEN_MIN > parse_limit {
            return current;
        }

        let first = ParseDecision {
            distance: node.reps[rep_index],
            kind: DecisionKind::Rep,
            length: rep_length as u32,
            rep_index: rep_index as u32,
        };
        let first_price = rep_prefix
            + self.pure_rep_price(rep_index as u32, node.state, pos_state)
            + self
                .rep_len_encoder
                .price(rep_length as u32, pos_state as usize);
        let (state_after_first, reps_after_first) =
            advance_decision_state(node.state, node.reps, first);
        let literal_position = base_position + current + rep_length;
        let literal_price = first_price
            + self.literal_transition_price(
                input,
                literal_position,
                state_after_first,
                reps_after_first,
            );
        let state_after_literal = state_update_literal(state_after_first);
        let rep_position = literal_position + 1;
        let tail_length = self.rep0_length_at(
            input,
            rep_position,
            end,
            reps_after_first[0],
            parse_limit - current - rep_length - 1,
        );
        if tail_length < MATCH_LEN_MIN {
            return current;
        }

        let tail_pos_state = self.pos_state(rep_position);
        let price = literal_price
            + self.rep0_match_price(state_after_literal, tail_pos_state, tail_length as u32);
        let literal = ParseDecision::literal();
        let tail = ParseDecision {
            distance: reps_after_first[0],
            kind: DecisionKind::Rep,
            length: tail_length as u32,
            rep_index: 0,
        };
        let (state, reps) = advance_decision_state(state_after_first, reps_after_first, literal);
        let (state, reps) = advance_decision_state(state, reps, tail);
        let edge = [first, literal, tail];
        let to = current + rep_length + 1 + tail_length;

        self.relax_optimal_edge(current, to, price, edge, 3, state, reps);
        to
    }

    #[allow(clippy::too_many_arguments)]
    fn price_match_literal_rep0_transition(
        &mut self,
        input: &[u8],
        end: usize,
        base_position: usize,
        current: usize,
        parse_limit: usize,
        normal_prefix: u32,
        node: OptimalNode,
        pos_state: u32,
        candidate: MatchCandidate,
        match_length: usize,
    ) -> usize {
        if current + match_length + 1 + MATCH_LEN_MIN > parse_limit {
            return current;
        }

        let first = ParseDecision {
            distance: candidate.distance,
            kind: DecisionKind::Match,
            length: match_length as u32,
            rep_index: 0,
        };
        let first_price = normal_prefix
            + self.dist_len_price(candidate.distance - 1, match_length as u32, pos_state);
        let (state_after_first, reps_after_first) =
            advance_decision_state(node.state, node.reps, first);
        let literal_position = base_position + current + match_length;
        let literal_price = first_price
            + self.literal_transition_price(
                input,
                literal_position,
                state_after_first,
                reps_after_first,
            );
        let state_after_literal = state_update_literal(state_after_first);
        let rep_position = literal_position + 1;
        let tail_length = self.rep0_length_at(
            input,
            rep_position,
            end,
            reps_after_first[0],
            parse_limit - current - match_length - 1,
        );
        if tail_length < MATCH_LEN_MIN {
            return current;
        }

        let tail_pos_state = self.pos_state(rep_position);
        let price = literal_price
            + self.rep0_match_price(state_after_literal, tail_pos_state, tail_length as u32);
        let literal = ParseDecision::literal();
        let tail = ParseDecision {
            distance: reps_after_first[0],
            kind: DecisionKind::Rep,
            length: tail_length as u32,
            rep_index: 0,
        };
        let (state, reps) = advance_decision_state(state_after_first, reps_after_first, literal);
        let (state, reps) = advance_decision_state(state, reps, tail);
        let edge = [first, literal, tail];
        let to = current + match_length + 1 + tail_length;

        self.relax_optimal_edge(current, to, price, edge, 3, state, reps);
        to
    }

    fn relax_optimal(
        &mut self,
        from: usize,
        to: usize,
        price: u32,
        decision: ParseDecision,
        state: u32,
        reps: [u32; 4],
    ) {
        let edge = [decision, ParseDecision::literal(), ParseDecision::literal()];
        self.relax_optimal_edge(from, to, price, edge, 1, state, reps);
    }

    #[allow(clippy::too_many_arguments)]
    fn relax_optimal_edge(
        &mut self,
        from: usize,
        to: usize,
        price: u32,
        edge: [ParseDecision; 3],
        edge_len: u8,
        state: u32,
        reps: [u32; 4],
    ) {
        debug_assert!(from < to);
        debug_assert!(to < self.opts.len());
        debug_assert!((1..=edge.len() as u8).contains(&edge_len));
        debug_assert_eq!(
            edge_total_length(&edge[..edge_len as usize]),
            (to - from) as u32
        );

        if price >= self.opts[to].price {
            return;
        }

        self.opts[to] = OptimalNode {
            edge,
            edge_len,
            pos_prev: from,
            price,
            reps,
            state,
        };
    }

    fn queue_optimal_path(&mut self, target: usize) -> ParseDecision {
        let mut index = target;
        let mut edges = Vec::new();
        self.pending_decisions.clear();
        self.pending_index = 0;

        while index > 0 {
            let node = self.opts[index];
            debug_assert!(node.pos_prev < index);
            debug_assert!(node.edge_len > 0);
            edges.push((node.edge, node.edge_len));
            index = node.pos_prev;
        }

        for (edge, edge_len) in edges.into_iter().rev() {
            self.pending_decisions
                .extend_from_slice(&edge[..edge_len as usize]);
        }

        self.next_pending_decision()
            .unwrap_or_else(ParseDecision::literal)
    }

    fn rep_matches(&self, input: &[u8], position: usize, end: usize) -> [MatchCandidate; 4] {
        let mut matches = [MatchCandidate::empty(); 4];

        for (index, slot) in matches.iter_mut().enumerate() {
            let distance = self.reps[index] as usize + 1;
            if position < self.dictionary_start + distance {
                continue;
            }

            if distance > self.dict_size as usize {
                continue;
            }

            let candidate = position - distance;
            let length = match_length(input, position, candidate, end, self.nice);
            if length > 0 {
                *slot = MatchCandidate {
                    distance: self.reps[index],
                    length: length as u32,
                    rep_index: index as u32,
                };
            }
        }

        matches
    }

    fn encode_decision(
        &mut self,
        range: &mut RangeEncoder,
        input: &[u8],
        position: usize,
        decision: ParseDecision,
    ) -> Result<()> {
        self.debug_validate_decision(input, position, decision);

        let pos_state = self.pos_state(position);
        let state = self.state as usize;
        let match_index = (state << NUM_POS_BITS_MAX) + pos_state as usize;

        match decision.kind {
            DecisionKind::Literal => self.encode_literal(range, input, position, match_index),
            DecisionKind::Match => self.encode_match(range, decision, match_index, pos_state),
            DecisionKind::Rep => self.encode_repetition(range, decision, match_index, pos_state),
        }
    }

    fn debug_validate_decision(&self, input: &[u8], position: usize, decision: ParseDecision) {
        if !cfg!(debug_assertions) {
            return;
        }

        match decision.kind {
            DecisionKind::Literal => {
                debug_assert_eq!(decision.length, 1);
            }
            DecisionKind::Match => {
                debug_assert!(decision.distance > 0);
                debug_assert!(decision.length >= MATCH_LEN_MIN as u32);
                debug_assert!(decision.length <= MATCH_LEN_MAX as u32);
                self.debug_validate_match(input, position, decision.distance - 1, decision.length);
            }
            DecisionKind::Rep => {
                debug_assert!(decision.rep_index < 4);
                debug_assert_eq!(decision.distance, self.reps[decision.rep_index as usize]);
                self.debug_validate_match(input, position, decision.distance, decision.length);
            }
        }
    }

    fn debug_validate_match(&self, input: &[u8], position: usize, distance: u32, length: u32) {
        let distance = distance as usize + 1;
        debug_assert!(distance <= self.dict_size as usize);
        debug_assert!(position >= self.dictionary_start + distance);

        let candidate = position - distance;
        for offset in 0..length as usize {
            debug_assert_eq!(
                input[position + offset],
                input[candidate + offset],
                "position={position} distance={} length={length} offset={offset}",
                distance - 1,
            );
        }
    }

    fn encode_literal(
        &mut self,
        range: &mut RangeEncoder,
        input: &[u8],
        position: usize,
        match_index: usize,
    ) -> Result<()> {
        range.encode_bit(&mut self.is_match, match_index, 0);

        let previous = if position == self.dictionary_start {
            0
        } else {
            input[position - 1]
        };
        let context = self.literal_context(position, previous);
        let offset = context * 0x300;

        if state_is_literal(self.state) {
            encode_literal_plain(
                range,
                &mut self.literal[offset..offset + 0x300],
                input[position],
            );
        } else {
            let match_byte = self.match_byte(input, position)?;
            encode_literal_matched(
                range,
                &mut self.literal[offset..offset + 0x300],
                input[position],
                match_byte,
            );
        }

        self.state = state_update_literal(self.state);
        Ok(())
    }

    fn encode_repetition(
        &mut self,
        range: &mut RangeEncoder,
        decision: ParseDecision,
        match_index: usize,
        pos_state: u32,
    ) -> Result<()> {
        range.encode_bit(&mut self.is_match, match_index, 1);
        range.encode_bit(&mut self.is_rep, self.state as usize, 1);

        if decision.rep_index == 0 {
            self.encode_rep0(range, decision, match_index, pos_state);
        } else {
            self.encode_rep_distance(range, decision.rep_index);
            self.rotate_reps(decision.rep_index as usize);
            self.rep_len_encoder
                .encode(range, pos_state, decision.length - 2);
            self.state = state_update_repetition(self.state);
        }

        Ok(())
    }

    fn encode_rep0(
        &mut self,
        range: &mut RangeEncoder,
        decision: ParseDecision,
        match_index: usize,
        pos_state: u32,
    ) {
        range.encode_bit(&mut self.is_rep_g0, self.state as usize, 0);

        if decision.length == 1 {
            range.encode_bit(&mut self.is_rep0_long, match_index, 0);
            self.state = state_update_short_rep(self.state);
            return;
        }

        range.encode_bit(&mut self.is_rep0_long, match_index, 1);
        self.rep_len_encoder
            .encode(range, pos_state, decision.length - 2);
        self.state = state_update_repetition(self.state);
    }

    fn encode_rep_distance(&mut self, range: &mut RangeEncoder, rep_index: u32) {
        range.encode_bit(&mut self.is_rep_g0, self.state as usize, 1);

        if rep_index == 1 {
            range.encode_bit(&mut self.is_rep_g1, self.state as usize, 0);
            return;
        }

        range.encode_bit(&mut self.is_rep_g1, self.state as usize, 1);

        if rep_index == 2 {
            range.encode_bit(&mut self.is_rep_g2, self.state as usize, 0);
        } else {
            range.encode_bit(&mut self.is_rep_g2, self.state as usize, 1);
        }
    }

    fn rotate_reps(&mut self, rep_index: usize) {
        let distance = self.reps[rep_index];

        if rep_index == 3 {
            self.reps[3] = self.reps[2];
        }

        if rep_index >= 2 {
            self.reps[2] = self.reps[1];
        }

        self.reps[1] = self.reps[0];
        self.reps[0] = distance;
    }

    fn encode_match(
        &mut self,
        range: &mut RangeEncoder,
        decision: ParseDecision,
        match_index: usize,
        pos_state: u32,
    ) -> Result<()> {
        debug_assert!(decision.distance > 0);

        range.encode_bit(&mut self.is_match, match_index, 1);
        range.encode_bit(&mut self.is_rep, self.state as usize, 0);
        self.len_encoder
            .encode(range, pos_state, decision.length - 2);
        self.encode_distance(range, decision.distance - 1, decision.length);
        self.match_price_count = self.match_price_count.saturating_add(1);

        self.reps[3] = self.reps[2];
        self.reps[2] = self.reps[1];
        self.reps[1] = self.reps[0];
        self.reps[0] = decision.distance - 1;
        self.state = state_update_match(self.state);

        Ok(())
    }

    fn encode_distance(&mut self, range: &mut RangeEncoder, distance: u32, length: u32) {
        let len_state = (length - 2).min((NUM_LEN_TO_POS_STATES - 1) as u32) as usize;
        let pos_slot = distance_to_pos_slot(distance);
        let offset = len_state * (1 << NUM_POS_SLOT_BITS);

        range.encode_bit_tree(
            &mut self.pos_slot[offset..offset + (1 << NUM_POS_SLOT_BITS)],
            NUM_POS_SLOT_BITS,
            pos_slot,
        );

        if pos_slot >= START_POS_MODEL_INDEX {
            self.encode_distance_footer(range, distance, pos_slot);
        }
    }

    fn encode_distance_footer(&mut self, range: &mut RangeEncoder, distance: u32, pos_slot: u32) {
        let direct_bits = (pos_slot >> 1) - 1;
        let base = (2 | (pos_slot & 1)) << direct_bits;
        let footer = distance - base;

        if pos_slot < END_POS_MODEL_INDEX {
            encode_distance_special(
                range,
                &mut self.pos_decoders,
                pos_slot,
                direct_bits,
                base,
                footer,
            );
        } else {
            range.encode_direct_bits(footer >> NUM_ALIGN_BITS, direct_bits - NUM_ALIGN_BITS);
            range.encode_reverse_bit_tree(&mut self.pos_align, NUM_ALIGN_BITS, footer & 0x0F);
            self.align_price_count = self.align_price_count.saturating_add(1);
        }
    }

    fn match_byte(&self, input: &[u8], position: usize) -> Result<u8> {
        let distance = self.reps[0] as usize + 1;
        if position < self.dictionary_start + distance {
            return Err(Error::Format("LZMA encoder missing match byte"));
        }

        Ok(input[position - distance])
    }

    fn literal_context(&self, position: usize, previous: u8) -> usize {
        let lp_mask = (1usize << self.properties.lp) - 1;
        let relative_position = position - self.dictionary_start;
        let position_bits = (relative_position & lp_mask) << self.properties.lc;
        let literal_bits = usize::from(previous >> (8 - self.properties.lc));

        position_bits + literal_bits
    }

    fn pos_state(&self, position: usize) -> u32 {
        ((position - self.dictionary_start) as u32) & ((1 << self.properties.pb) - 1)
    }

    fn match_index(&self, state: u32, pos_state: u32) -> usize {
        (state as usize * NUM_POS_STATES_MAX) + pos_state as usize
    }

    fn rep_length(
        &self,
        input: &[u8],
        position: usize,
        end: usize,
        rep: u32,
        available: usize,
    ) -> usize {
        let distance = rep as usize + 1;
        if position < self.dictionary_start + distance {
            return 0;
        }

        if distance > self.dict_size as usize {
            return 0;
        }

        let candidate = position - distance;
        let limit = available.min(self.nice);

        match_length(input, position, candidate, end, limit)
    }

    fn rep0_length_at(
        &self,
        input: &[u8],
        position: usize,
        end: usize,
        rep0: u32,
        available: usize,
    ) -> usize {
        if available < MATCH_LEN_MIN || position >= end {
            return 0;
        }

        let distance = rep0 as usize + 1;
        if position < self.dictionary_start + distance {
            return 0;
        }

        if distance > self.dict_size as usize {
            return 0;
        }

        let candidate = position - distance;
        let limit = available
            .min(self.nice)
            .min(MATCH_LEN_MAX)
            .min(end - position);

        match_length_from(input, position, candidate, end, limit, 0)
    }

    fn literal_transition_price(
        &self,
        input: &[u8],
        position: usize,
        state: u32,
        reps: [u32; 4],
    ) -> u32 {
        let pos_state = self.pos_state(position);
        let match_index = self.match_index(state, pos_state);

        rc_bit_0_price(self.is_match[match_index])
            + self.literal_price(input, position, state, reps)
    }

    fn rep0_match_price(&self, state: u32, pos_state: u32, length: u32) -> u32 {
        let match_index = self.match_index(state, pos_state);

        rc_bit_1_price(self.is_match[match_index])
            + rc_bit_1_price(self.is_rep[state as usize])
            + self.pure_rep_price(0, state, pos_state)
            + self.rep_len_encoder.price(length, pos_state as usize)
    }

    fn literal_price(&self, input: &[u8], position: usize, state: u32, reps: [u32; 4]) -> u32 {
        let previous = if position == self.dictionary_start {
            0
        } else {
            input[position - 1]
        };
        let context = self.literal_context(position, previous);
        let offset = context * 0x300;
        let probs = &self.literal[offset..offset + 0x300];

        if state_is_literal(state) {
            return literal_plain_price(probs, input[position]);
        }

        let distance = reps[0] as usize + 1;
        if position < self.dictionary_start + distance {
            return RC_INFINITY_PRICE / 4;
        }

        literal_matched_price(probs, input[position], input[position - distance])
    }

    fn pure_rep_price(&self, rep_index: u32, state: u32, pos_state: u32) -> u32 {
        let state_index = state as usize;
        if rep_index == 0 {
            let match_index = self.match_index(state, pos_state);
            return rc_bit_0_price(self.is_rep_g0[state_index])
                + rc_bit_1_price(self.is_rep0_long[match_index]);
        }

        if rep_index == 1 {
            return rc_bit_1_price(self.is_rep_g0[state_index])
                + rc_bit_0_price(self.is_rep_g1[state_index]);
        }

        rc_bit_1_price(self.is_rep_g0[state_index])
            + rc_bit_1_price(self.is_rep_g1[state_index])
            + rc_bit_price(self.is_rep_g2[state_index], rep_index - 2)
    }

    fn dist_len_price(&self, distance: u32, length: u32, pos_state: u32) -> u32 {
        let dist_state = dist_state(length);
        let distance_price = if distance < NUM_FULL_DISTANCES as u32 {
            self.dist_prices[dist_state * NUM_FULL_DISTANCES + distance as usize]
        } else {
            let pos_slot = distance_to_pos_slot(distance);
            let slot_index = dist_state * (1 << NUM_POS_SLOT_BITS) + pos_slot as usize;

            self.dist_slot_prices[slot_index] + self.align_prices[(distance & 0x0F) as usize]
        };

        distance_price + self.len_encoder.price(length, pos_state as usize)
    }

    fn refresh_price_tables(&mut self) {
        if self.match_price_count >= MATCH_PRICE_REFRESH {
            self.fill_dist_prices();
        }

        if self.align_price_count >= 1 << NUM_ALIGN_BITS {
            self.fill_align_prices();
        }
    }

    fn fill_dist_prices(&mut self) {
        for state in 0..NUM_LEN_TO_POS_STATES {
            let slot_offset = state * (1 << NUM_POS_SLOT_BITS);
            let probs = &self.pos_slot[slot_offset..slot_offset + (1 << NUM_POS_SLOT_BITS)];

            for slot in 0..self.dist_table_size {
                self.dist_slot_prices[slot_offset + slot] =
                    bit_tree_price(probs, NUM_POS_SLOT_BITS, slot as u32);
            }

            for slot in END_POS_MODEL_INDEX as usize..self.dist_table_size {
                let direct_bits = ((slot as u32 >> 1) - 1) - NUM_ALIGN_BITS;
                self.dist_slot_prices[slot_offset + slot] += rc_direct_price(direct_bits);
            }

            let dist_offset = state * NUM_FULL_DISTANCES;
            for distance in 0..START_POS_MODEL_INDEX as usize {
                self.dist_prices[dist_offset + distance] =
                    self.dist_slot_prices[slot_offset + distance];
            }
        }

        self.fill_full_distance_prices();
        self.match_price_count = 0;
    }

    fn fill_full_distance_prices(&mut self) {
        for distance in START_POS_MODEL_INDEX as usize..NUM_FULL_DISTANCES {
            let pos_slot = distance_to_pos_slot(distance as u32);
            let direct_bits = (pos_slot >> 1) - 1;
            let base = (2 | (pos_slot & 1)) << direct_bits;
            let footer = distance as u32 - base;
            let footer_price =
                distance_special_price(&self.pos_decoders, pos_slot, direct_bits, base, footer);

            for state in 0..NUM_LEN_TO_POS_STATES {
                let slot_offset = state * (1 << NUM_POS_SLOT_BITS);
                let dist_offset = state * NUM_FULL_DISTANCES;

                self.dist_prices[dist_offset + distance] =
                    self.dist_slot_prices[slot_offset + pos_slot as usize] + footer_price;
            }
        }
    }

    fn fill_align_prices(&mut self) {
        for value in 0..self.align_prices.len() {
            self.align_prices[value] =
                reverse_bit_tree_price(&self.pos_align, NUM_ALIGN_BITS, value as u32);
        }

        self.align_price_count = 0;
    }
}

pub(crate) struct LzmaEncoderState {
    align_price_count: u32,
    align_prices: [u32; 1 << NUM_ALIGN_BITS],
    dist_prices: Vec<u32>,
    dist_slot_prices: Vec<u32>,
    is_match: Vec<u16>,
    is_rep: [u16; NUM_STATES],
    is_rep_g0: [u16; NUM_STATES],
    is_rep_g1: [u16; NUM_STATES],
    is_rep_g2: [u16; NUM_STATES],
    is_rep0_long: Vec<u16>,
    len_encoder: LenDecoder,
    literal: Vec<u16>,
    match_price_count: u32,
    pos_align: [u16; 1 << NUM_ALIGN_BITS],
    pos_decoders: [u16; NUM_FULL_DISTANCES],
    pos_slot: Vec<u16>,
    rep_len_encoder: LenDecoder,
    reps: [u32; 4],
    state: u32,
}

pub struct LzmaDecoder {
    dict_size: u32,
    is_match: Vec<u16>,
    is_rep: [u16; NUM_STATES],
    is_rep_g0: [u16; NUM_STATES],
    is_rep_g1: [u16; NUM_STATES],
    is_rep_g2: [u16; NUM_STATES],
    is_rep0_long: Vec<u16>,
    len_decoder: LenDecoder,
    literal: Vec<u16>,
    pos_align: [u16; 1 << NUM_ALIGN_BITS],
    pos_decoders: [u16; NUM_FULL_DISTANCES],
    pos_slot: Vec<u16>,
    properties: LzmaProperties,
    rep_len_decoder: LenDecoder,
    reps: [u32; 4],
    state: u32,
}

impl LzmaDecoder {
    pub fn new(properties: LzmaProperties, dict_size: u32) -> LzmaDecoder {
        let literal_contexts = 1usize << (properties.lc + properties.lp);

        let mut decoder = LzmaDecoder {
            dict_size,
            is_match: vec![0; NUM_STATES * NUM_POS_STATES_MAX],
            is_rep: [0; NUM_STATES],
            is_rep_g0: [0; NUM_STATES],
            is_rep_g1: [0; NUM_STATES],
            is_rep_g2: [0; NUM_STATES],
            is_rep0_long: vec![0; NUM_STATES * NUM_POS_STATES_MAX],
            len_decoder: LenDecoder::new(),
            literal: vec![0; literal_contexts * 0x300],
            pos_align: [0; 1 << NUM_ALIGN_BITS],
            pos_decoders: [0; NUM_FULL_DISTANCES],
            pos_slot: vec![0; NUM_LEN_TO_POS_STATES * (1 << NUM_POS_SLOT_BITS)],
            properties,
            rep_len_decoder: LenDecoder::new(),
            reps: [0; 4],
            state: 0,
        };

        decoder.reset_state();
        decoder
    }

    pub fn reset_state(&mut self) {
        fill_probs(&mut self.is_match);
        fill_probs(&mut self.is_rep);
        fill_probs(&mut self.is_rep_g0);
        fill_probs(&mut self.is_rep_g1);
        fill_probs(&mut self.is_rep_g2);
        fill_probs(&mut self.is_rep0_long);
        fill_probs(&mut self.literal);
        fill_probs(&mut self.pos_align);
        fill_probs(&mut self.pos_decoders);
        fill_probs(&mut self.pos_slot);

        self.len_decoder.reset();
        self.rep_len_decoder.reset();
        self.reps = [0; 4];
        self.state = 0;
    }

    pub fn decode_chunk(
        &mut self,
        input: &[u8],
        output: &mut Vec<u8>,
        unpack_size: usize,
        dictionary_start: usize,
    ) -> Result<()> {
        let mut range = RangeDecoder::new(input)?;
        let target = output
            .len()
            .checked_add(unpack_size)
            .ok_or(Error::Format("LZMA output size overflow"))?;

        while output.len() < target {
            let pos_state =
                ((output.len() - dictionary_start) as u32) & ((1 << self.properties.pb) - 1);
            let state = self.state as usize;
            let match_index = (state << NUM_POS_BITS_MAX) + pos_state as usize;

            if range.decode_bit(&mut self.is_match, match_index)? == 0 {
                self.decode_literal(&mut range, output, dictionary_start)?;
            } else if range.decode_bit(&mut self.is_rep, state)? == 0 {
                self.decode_match(&mut range, output, target, dictionary_start, pos_state)?;
            } else {
                self.decode_repetition(&mut range, output, target, dictionary_start, pos_state)?;
            }
        }

        Ok(())
    }

    fn decode_literal(
        &mut self,
        range: &mut RangeDecoder<'_>,
        output: &mut Vec<u8>,
        dictionary_start: usize,
    ) -> Result<()> {
        let previous = if output.len() == dictionary_start {
            0
        } else {
            output.last().copied().unwrap_or(0)
        };
        let context = self.literal_context(output.len() - dictionary_start, previous);
        let offset = context * 0x300;
        let symbol = if state_is_literal(self.state) {
            decode_literal_plain(range, &mut self.literal[offset..offset + 0x300])?
        } else {
            let match_byte = self.dictionary_byte(output, self.reps[0], dictionary_start)?;
            decode_literal_matched(range, &mut self.literal[offset..offset + 0x300], match_byte)?
        };

        output.push(symbol);
        self.state = state_update_literal(self.state);

        Ok(())
    }

    fn decode_match(
        &mut self,
        range: &mut RangeDecoder<'_>,
        output: &mut Vec<u8>,
        target: usize,
        dictionary_start: usize,
        pos_state: u32,
    ) -> Result<()> {
        let length = self.len_decoder.decode(range, pos_state)? + 2;
        let distance = self.decode_distance(range, length)?;

        self.reps[3] = self.reps[2];
        self.reps[2] = self.reps[1];
        self.reps[1] = self.reps[0];
        self.reps[0] = distance;
        self.state = state_update_match(self.state);

        self.copy_match(output, target, dictionary_start, distance, length)
    }

    fn decode_repetition(
        &mut self,
        range: &mut RangeDecoder<'_>,
        output: &mut Vec<u8>,
        target: usize,
        dictionary_start: usize,
        pos_state: u32,
    ) -> Result<()> {
        let state = self.state as usize;
        let distance;

        if range.decode_bit(&mut self.is_rep_g0, state)? == 0 {
            let index = (state << NUM_POS_BITS_MAX) + pos_state as usize;

            if range.decode_bit(&mut self.is_rep0_long, index)? == 0 {
                distance = self.reps[0];
                self.state = state_update_short_rep(self.state);
                return self.copy_match(output, target, dictionary_start, distance, 1);
            }

            distance = self.reps[0];
        } else {
            distance = if range.decode_bit(&mut self.is_rep_g1, state)? == 0 {
                self.reps[1]
            } else if range.decode_bit(&mut self.is_rep_g2, state)? == 0 {
                let distance = self.reps[2];
                self.reps[2] = self.reps[1];
                distance
            } else {
                let distance = self.reps[3];
                self.reps[3] = self.reps[2];
                self.reps[2] = self.reps[1];
                distance
            };

            self.reps[1] = self.reps[0];
            self.reps[0] = distance;
        }

        let length = self.rep_len_decoder.decode(range, pos_state)? + 2;

        self.state = state_update_repetition(self.state);
        self.copy_match(output, target, dictionary_start, distance, length)
    }

    fn decode_distance(&mut self, range: &mut RangeDecoder<'_>, length: u32) -> Result<u32> {
        let len_state = (length - 2).min((NUM_LEN_TO_POS_STATES - 1) as u32) as usize;
        let offset = len_state * (1 << NUM_POS_SLOT_BITS);
        let pos_slot = range.decode_bit_tree(
            &mut self.pos_slot[offset..offset + (1 << NUM_POS_SLOT_BITS)],
            NUM_POS_SLOT_BITS,
        )?;

        if pos_slot < START_POS_MODEL_INDEX {
            return Ok(pos_slot);
        }

        let direct_bits = (pos_slot >> 1) - 1;
        let mut distance = (2 | (pos_slot & 1)) << direct_bits;

        if pos_slot < END_POS_MODEL_INDEX {
            let reverse = decode_distance_special(
                range,
                &mut self.pos_decoders,
                pos_slot,
                direct_bits,
                distance,
            )?;
            distance += reverse;
        } else {
            let direct = range.decode_direct_bits(direct_bits - NUM_ALIGN_BITS)?;
            distance += direct << NUM_ALIGN_BITS;
            distance += range.decode_reverse_bit_tree(&mut self.pos_align, NUM_ALIGN_BITS)?;
        }

        Ok(distance)
    }

    fn copy_match(
        &self,
        output: &mut Vec<u8>,
        target: usize,
        dictionary_start: usize,
        distance: u32,
        length: u32,
    ) -> Result<()> {
        if distance >= self.dict_size {
            return Err(Error::Format("LZMA distance exceeds dictionary"));
        }

        let distance = distance as usize + 1;
        let length = length as usize;
        let output_len = output.len();
        let Some(match_end) = output_len.checked_add(length) else {
            return Err(Error::Format("LZMA output size overflow"));
        };

        if match_end > target {
            return Err(Error::Format("LZMA match exceeds chunk size"));
        }

        if output_len < dictionary_start + distance {
            return Err(Error::Format("LZMA match before dictionary"));
        }

        let start = output_len - distance;
        if distance == 1 {
            let byte = output[start];
            output.resize(match_end, byte);
            return Ok(());
        }

        if length <= distance && length >= 4 {
            output.extend_from_within(start..start + length);
            return Ok(());
        }

        for index in 0..length {
            output.push(output[start + index]);
        }

        Ok(())
    }

    fn dictionary_byte(&self, output: &[u8], distance: u32, dictionary_start: usize) -> Result<u8> {
        let distance = distance as usize + 1;

        if output.len() < dictionary_start + distance {
            return Err(Error::Format("LZMA match before dictionary"));
        }

        Ok(output[output.len() - distance])
    }

    fn literal_context(&self, position: usize, previous: u8) -> usize {
        let lp_mask = (1usize << self.properties.lp) - 1;
        let position_bits = (position & lp_mask) << self.properties.lc;
        let literal_bits = usize::from(previous >> (8 - self.properties.lc));

        position_bits + literal_bits
    }
}

#[derive(Clone)]
struct LenDecoder {
    choice: [u16; 2],
    counters: [u32; NUM_POS_STATES_MAX],
    high: [u16; 1 << 8],
    low: [u16; NUM_POS_STATES_MAX * (1 << 3)],
    mid: [u16; NUM_POS_STATES_MAX * (1 << 3)],
    prices: Vec<u32>,
    table_size: usize,
}

impl LenDecoder {
    fn new() -> LenDecoder {
        LenDecoder {
            choice: [0; 2],
            counters: [0; NUM_POS_STATES_MAX],
            high: [0; 1 << 8],
            low: [0; NUM_POS_STATES_MAX * (1 << 3)],
            mid: [0; NUM_POS_STATES_MAX * (1 << 3)],
            prices: vec![0; NUM_POS_STATES_MAX * LEN_SYMBOLS],
            table_size: LEN_SYMBOLS,
        }
    }

    fn set_table_size(&mut self, table_size: usize) {
        self.table_size = table_size.clamp(1, LEN_SYMBOLS);
    }

    fn reset(&mut self) {
        fill_probs(&mut self.choice);
        fill_probs(&mut self.high);
        fill_probs(&mut self.low);
        fill_probs(&mut self.mid);

        for pos_state in 0..NUM_POS_STATES_MAX {
            self.update_prices(pos_state);
        }
    }

    fn decode(&mut self, range: &mut RangeDecoder<'_>, pos_state: u32) -> Result<u32> {
        if range.decode_bit(&mut self.choice, 0)? == 0 {
            let offset = pos_state as usize * (1 << 3);
            return range.decode_bit_tree(&mut self.low[offset..offset + (1 << 3)], 3);
        }

        if range.decode_bit(&mut self.choice, 1)? == 0 {
            let offset = pos_state as usize * (1 << 3);
            return Ok(8 + range.decode_bit_tree(&mut self.mid[offset..offset + (1 << 3)], 3)?);
        }

        Ok(16 + range.decode_bit_tree(&mut self.high, 8)?)
    }

    fn encode(&mut self, range: &mut RangeEncoder, pos_state: u32, symbol: u32) {
        if symbol < 8 {
            range.encode_bit(&mut self.choice, 0, 0);
            let offset = pos_state as usize * (1 << 3);
            range.encode_bit_tree(&mut self.low[offset..offset + (1 << 3)], 3, symbol);
        } else if symbol < 16 {
            range.encode_bit(&mut self.choice, 0, 1);
            range.encode_bit(&mut self.choice, 1, 0);
            let offset = pos_state as usize * (1 << 3);
            range.encode_bit_tree(&mut self.mid[offset..offset + (1 << 3)], 3, symbol - 8);
        } else {
            range.encode_bit(&mut self.choice, 0, 1);
            range.encode_bit(&mut self.choice, 1, 1);
            range.encode_bit_tree(&mut self.high, 8, symbol - 16);
        }

        self.update_after_encode(pos_state as usize);
    }

    fn price(&self, length: u32, pos_state: usize) -> u32 {
        debug_assert!(length >= MATCH_LEN_MIN as u32);
        debug_assert!(length <= MATCH_LEN_MAX as u32);
        debug_assert!(pos_state < NUM_POS_STATES_MAX);

        let symbol = length as usize - MATCH_LEN_MIN;
        self.prices[pos_state * LEN_SYMBOLS + symbol]
    }

    fn update_after_encode(&mut self, pos_state: usize) {
        debug_assert!(pos_state < NUM_POS_STATES_MAX);

        if self.counters[pos_state] > 0 {
            self.counters[pos_state] -= 1;
        }

        if self.counters[pos_state] == 0 {
            self.update_prices(pos_state);
        }
    }

    fn update_prices(&mut self, pos_state: usize) {
        debug_assert!(pos_state < NUM_POS_STATES_MAX);

        let a0 = rc_bit_0_price(self.choice[0]);
        let a1 = rc_bit_1_price(self.choice[0]);
        let b0 = a1 + rc_bit_0_price(self.choice[1]);
        let b1 = a1 + rc_bit_1_price(self.choice[1]);
        let prices = pos_state * LEN_SYMBOLS;
        let low = pos_state * (1 << 3);

        for symbol in 0..LEN_SYMBOLS {
            self.prices[prices + symbol] = if symbol < 8 {
                a0 + bit_tree_price(&self.low[low..low + (1 << 3)], 3, symbol as u32)
            } else if symbol < 16 {
                b0 + bit_tree_price(&self.mid[low..low + (1 << 3)], 3, symbol as u32 - 8)
            } else {
                b1 + bit_tree_price(&self.high, 8, symbol as u32 - 16)
            };
        }

        self.counters[pos_state] = self.table_size as u32;
    }
}

struct RangeEncoder {
    cache: u8,
    cache_size: u32,
    low: u64,
    output: Vec<u8>,
    output_limit: Option<usize>,
    output_limit_reached: bool,
    range: u32,
}

impl RangeEncoder {
    fn new(output_limit: Option<usize>) -> RangeEncoder {
        RangeEncoder {
            cache: 0,
            cache_size: 1,
            low: 0,
            output: Vec::new(),
            output_limit,
            output_limit_reached: false,
            range: u32::MAX,
        }
    }

    fn encode_bit(&mut self, probs: &mut [u16], index: usize, bit: u32) {
        debug_assert!(index < probs.len());
        debug_assert!(bit <= 1);

        let prob = u32::from(probs[index]);
        let bound = (self.range >> 11) * prob;

        if bit == 0 {
            self.range = bound;
            probs[index] = (prob + ((BIT_MODEL_TOTAL - prob) >> MOVE_BITS)) as u16;
        } else {
            self.low += u64::from(bound);
            self.range -= bound;
            probs[index] = (prob - (prob >> MOVE_BITS)) as u16;
        }

        if self.range < (1 << 24) {
            self.range <<= 8;
            self.shift_low();
        }
    }

    fn encode_bit_tree(&mut self, probs: &mut [u16], bits: u32, value: u32) {
        let mut symbol = 1u32;

        for index in (0..bits).rev() {
            let bit = (value >> index) & 1;
            self.encode_bit(probs, symbol as usize, bit);
            symbol = (symbol << 1) | bit;
        }
    }

    fn encode_reverse_bit_tree(&mut self, probs: &mut [u16], bits: u32, value: u32) {
        let mut symbol = 1u32;

        for index in 0..bits {
            let bit = (value >> index) & 1;
            self.encode_bit(probs, symbol as usize, bit);
            symbol = (symbol << 1) | bit;
        }
    }

    fn encode_direct_bits(&mut self, value: u32, bits: u32) {
        for index in (0..bits).rev() {
            self.range >>= 1;
            if ((value >> index) & 1) == 1 {
                self.low += u64::from(self.range);
            }

            if self.range < (1 << 24) {
                self.range <<= 8;
                self.shift_low();
            }
        }
    }

    fn finish(mut self) -> Option<Vec<u8>> {
        for _ in 0..5 {
            self.shift_low();
            if self.output_limit_reached() {
                return None;
            }
        }

        Some(self.output)
    }

    fn output_limit_reached(&self) -> bool {
        self.output_limit_reached
    }

    fn shift_low(&mut self) {
        let low = self.low as u32;
        let high = (self.low >> 32) as u8;

        if low < 0xFF00_0000 || high != 0 {
            let mut byte = self.cache;
            loop {
                self.output.push(byte.wrapping_add(high));
                self.check_output_limit();
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }

                byte = 0xFF;
            }

            self.cache = (low >> 24) as u8;
        }

        self.cache_size += 1;
        self.low = u64::from(low << 8);
    }

    fn check_output_limit(&mut self) {
        if let Some(limit) = self.output_limit
            && self.output.len() > limit
        {
            self.output_limit_reached = true;
        }
    }
}

struct RangeDecoder<'a> {
    code: u32,
    data: &'a [u8],
    index: usize,
    range: u32,
}

impl<'a> RangeDecoder<'a> {
    fn new(data: &'a [u8]) -> Result<RangeDecoder<'a>> {
        if data.len() < 5 {
            return Err(Error::Format("LZMA range stream is too short"));
        }

        if data[0] != 0 {
            return Err(Error::Format("LZMA range stream has invalid marker"));
        }

        let mut code = 0u32;
        for &byte in &data[..5] {
            code = (code << 8) | u32::from(byte);
        }

        Ok(RangeDecoder {
            code,
            data,
            index: 5,
            range: u32::MAX,
        })
    }

    fn decode_bit(&mut self, probs: &mut [u16], index: usize) -> Result<u32> {
        debug_assert!(index < probs.len());

        let prob_slot = unsafe { probs.get_unchecked_mut(index) };
        let prob = u32::from(*prob_slot);
        let bound = (self.range >> 11) * prob;
        let bit;

        if self.code < bound {
            self.range = bound;
            *prob_slot = (prob + ((BIT_MODEL_TOTAL - prob) >> MOVE_BITS)) as u16;
            bit = 0;
        } else {
            self.range -= bound;
            self.code -= bound;
            *prob_slot = (prob - (prob >> MOVE_BITS)) as u16;
            bit = 1;
        }

        self.normalize()?;

        Ok(bit)
    }

    fn decode_bit_tree(&mut self, probs: &mut [u16], bits: u32) -> Result<u32> {
        let mut symbol = 1u32;

        for _ in 0..bits {
            let bit = self.decode_bit(probs, symbol as usize)?;
            symbol = (symbol << 1) | bit;
        }

        Ok(symbol - (1 << bits))
    }

    fn decode_reverse_bit_tree(&mut self, probs: &mut [u16], bits: u32) -> Result<u32> {
        let mut result = 0u32;
        let mut symbol = 1u32;

        for index in 0..bits {
            let bit = self.decode_bit(probs, symbol as usize)?;
            symbol = (symbol << 1) | bit;
            result |= bit << index;
        }

        Ok(result)
    }

    fn decode_direct_bits(&mut self, bits: u32) -> Result<u32> {
        let mut result = 0u32;

        for _ in 0..bits {
            self.range >>= 1;
            self.code = self.code.wrapping_sub(self.range);

            let bit = if self.code >> 31 == 0 {
                1
            } else {
                self.code = self.code.wrapping_add(self.range);
                0
            };

            self.normalize()?;
            result = (result << 1) | bit;
        }

        Ok(result)
    }

    fn normalize(&mut self) -> Result<()> {
        if self.range >= (1 << 24) {
            return Ok(());
        }

        self.range <<= 8;
        self.code = (self.code << 8) | u32::from(self.read_byte()?);

        Ok(())
    }

    fn read_byte(&mut self) -> Result<u8> {
        if self.index < self.data.len() {
            let byte = self.data[self.index];
            self.index += 1;
            Ok(byte)
        } else {
            Err(Error::Format("truncated LZMA range stream"))
        }
    }
}

fn decode_literal_plain(range: &mut RangeDecoder<'_>, probs: &mut [u16]) -> Result<u8> {
    let mut symbol = 1u32;

    while symbol < 0x100 {
        let bit = range.decode_bit(probs, symbol as usize)?;
        symbol = (symbol << 1) | bit;
    }

    Ok(symbol as u8)
}

fn decode_literal_matched(
    range: &mut RangeDecoder<'_>,
    probs: &mut [u16],
    match_byte: u8,
) -> Result<u8> {
    let mut match_word = u32::from(match_byte);
    let mut offset = 0x100u32;
    let mut symbol = 1u32;

    while symbol < 0x100 {
        match_word <<= 1;
        let bit_base = offset;
        offset &= match_word;

        let bit = range.decode_bit(probs, (offset + bit_base + symbol) as usize)?;
        symbol = (symbol << 1) | bit;

        if bit == 0 {
            offset ^= bit_base;
        }
    }

    Ok(symbol as u8)
}

fn encode_literal_plain(range: &mut RangeEncoder, probs: &mut [u16], byte: u8) {
    let mut symbol = 0x100 | u32::from(byte);

    while symbol < 0x1_0000 {
        let bit = (symbol >> 7) & 1;
        range.encode_bit(probs, (symbol >> 8) as usize, bit);
        symbol <<= 1;
    }
}

fn encode_literal_matched(range: &mut RangeEncoder, probs: &mut [u16], byte: u8, match_byte: u8) {
    let mut match_word = u32::from(match_byte);
    let mut offset = 0x100u32;
    let mut symbol = 0x100 | u32::from(byte);

    while symbol < 0x1_0000 {
        match_word <<= 1;
        let bit = (symbol >> 7) & 1;
        let index = offset + (match_word & offset) + (symbol >> 8);

        symbol <<= 1;
        offset &= !(match_word ^ symbol);
        range.encode_bit(probs, index as usize, bit);
    }
}

const RC_PRICES: [u8; RC_PRICE_TABLE_SIZE] = build_rc_prices();

const fn build_rc_prices() -> [u8; RC_PRICE_TABLE_SIZE] {
    let mut prices = [0u8; RC_PRICE_TABLE_SIZE];
    let mut index = (1 << RC_MOVE_REDUCING_BITS) / 2;

    while index < BIT_MODEL_TOTAL {
        let mut weight = index;
        let mut bit_count = 0u32;
        let mut cycle = 0u32;

        while cycle < RC_BIT_PRICE_SHIFT_BITS {
            weight *= weight;
            bit_count <<= 1;

            while weight >= 1 << 16 {
                weight >>= 1;
                bit_count += 1;
            }

            cycle += 1;
        }

        prices[(index >> RC_MOVE_REDUCING_BITS) as usize] =
            ((11 << RC_BIT_PRICE_SHIFT_BITS) - 15 - bit_count) as u8;
        index += 1 << RC_MOVE_REDUCING_BITS;
    }

    prices
}

fn rc_bit_price(prob: u16, bit: u32) -> u32 {
    debug_assert!(bit <= 1);

    let prob = u32::from(prob);
    let index = (prob ^ (0u32.wrapping_sub(bit) & (BIT_MODEL_TOTAL - 1))) >> RC_MOVE_REDUCING_BITS;

    u32::from(RC_PRICES[index as usize])
}

fn rc_bit_0_price(prob: u16) -> u32 {
    u32::from(RC_PRICES[(u32::from(prob) >> RC_MOVE_REDUCING_BITS) as usize])
}

fn rc_bit_1_price(prob: u16) -> u32 {
    let index = (u32::from(prob) ^ (BIT_MODEL_TOTAL - 1)) >> RC_MOVE_REDUCING_BITS;

    u32::from(RC_PRICES[index as usize])
}

fn rc_direct_price(bits: u32) -> u32 {
    bits << RC_BIT_PRICE_SHIFT_BITS
}

fn bit_tree_price(probs: &[u16], bits: u32, value: u32) -> u32 {
    let mut price = 0u32;
    let mut symbol = value + (1 << bits);

    while symbol != 1 {
        let bit = symbol & 1;
        symbol >>= 1;
        price += rc_bit_price(probs[symbol as usize], bit);
    }

    price
}

fn reverse_bit_tree_price(probs: &[u16], bits: u32, value: u32) -> u32 {
    let mut price = 0u32;
    let mut model_index = 1u32;
    let mut symbol = value;

    for _ in 0..bits {
        let bit = symbol & 1;
        symbol >>= 1;
        price += rc_bit_price(probs[model_index as usize], bit);
        model_index = (model_index << 1) | bit;
    }

    price
}

fn encode_distance_special(
    range: &mut RangeEncoder,
    probs: &mut [u16],
    pos_slot: u32,
    bits: u32,
    base: u32,
    value: u32,
) {
    let mut symbol = 1u32;

    for index in 0..bits {
        let bit = (value >> index) & 1;
        range.encode_bit(probs, distance_special_index(base, pos_slot, symbol), bit);
        symbol = (symbol << 1) | bit;
    }
}

fn decode_distance_special(
    range: &mut RangeDecoder<'_>,
    probs: &mut [u16],
    pos_slot: u32,
    bits: u32,
    base: u32,
) -> Result<u32> {
    let mut result = 0u32;
    let mut symbol = 1u32;

    for index in 0..bits {
        let bit = range.decode_bit(probs, distance_special_index(base, pos_slot, symbol))?;
        symbol = (symbol << 1) | bit;
        result |= bit << index;
    }

    Ok(result)
}

fn distance_special_price(probs: &[u16], pos_slot: u32, bits: u32, base: u32, value: u32) -> u32 {
    let mut price = 0u32;
    let mut symbol = 1u32;

    for index in 0..bits {
        let bit = (value >> index) & 1;
        price += rc_bit_price(probs[distance_special_index(base, pos_slot, symbol)], bit);
        symbol = (symbol << 1) | bit;
    }

    price
}

fn distance_special_index(base: u32, pos_slot: u32, symbol: u32) -> usize {
    let index = base + symbol - pos_slot - 1;
    debug_assert!(index < NUM_FULL_DISTANCES as u32 - END_POS_MODEL_INDEX);

    index as usize
}

fn literal_plain_price(probs: &[u16], byte: u8) -> u32 {
    bit_tree_price(probs, 8, u32::from(byte))
}

fn literal_matched_price(probs: &[u16], byte: u8, match_byte: u8) -> u32 {
    let mut match_word = u32::from(match_byte);
    let mut offset = 0x100u32;
    let mut price = 0u32;
    let mut symbol = 0x100 | u32::from(byte);

    while symbol < 0x1_0000 {
        match_word <<= 1;
        let bit = (symbol >> 7) & 1;
        let index = offset + (match_word & offset) + (symbol >> 8);

        symbol <<= 1;
        offset &= !(match_word ^ symbol);
        price += rc_bit_price(probs[index as usize], bit);
    }

    price
}

#[derive(Clone, Copy, Debug)]
struct ParseDecision {
    distance: u32,
    kind: DecisionKind,
    length: u32,
    rep_index: u32,
}

impl ParseDecision {
    fn literal() -> ParseDecision {
        ParseDecision {
            distance: 0,
            kind: DecisionKind::Literal,
            length: 1,
            rep_index: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct OptimalNode {
    edge: [ParseDecision; 3],
    edge_len: u8,
    pos_prev: usize,
    price: u32,
    reps: [u32; 4],
    state: u32,
}

impl OptimalNode {
    fn empty() -> OptimalNode {
        OptimalNode {
            edge: [ParseDecision::literal(); 3],
            edge_len: 0,
            pos_prev: 0,
            price: RC_INFINITY_PRICE,
            reps: [0; 4],
            state: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DecisionKind {
    Literal,
    Match,
    Rep,
}

#[derive(Clone, Copy, Debug)]
struct MatchCandidate {
    distance: u32,
    length: u32,
    rep_index: u32,
}

impl MatchCandidate {
    fn empty() -> MatchCandidate {
        MatchCandidate {
            distance: 0,
            length: 0,
            rep_index: 0,
        }
    }
}

struct MatchList {
    count: usize,
    matches: [MatchCandidate; MATCH_LEN_MAX + 1],
}

impl MatchList {
    fn new() -> MatchList {
        MatchList {
            count: 0,
            matches: [MatchCandidate::empty(); MATCH_LEN_MAX + 1],
        }
    }

    fn best(&self) -> Option<MatchCandidate> {
        if self.count == 0 {
            None
        } else {
            Some(self.matches[self.count - 1])
        }
    }

    fn iter(&self) -> impl Iterator<Item = MatchCandidate> + '_ {
        self.matches[..self.count].iter().copied()
    }

    fn push(&mut self, distance: u32, length: u32) {
        debug_assert!(length >= MATCH_LEN_MIN as u32);
        debug_assert!(length <= MATCH_LEN_MAX as u32);

        if self.count > 0 {
            let last = &mut self.matches[self.count - 1];
            if length < last.length {
                return;
            }

            if length == last.length {
                if distance < last.distance {
                    last.distance = distance;
                }

                return;
            }
        }

        debug_assert!(self.count < self.matches.len());
        self.matches[self.count] = MatchCandidate {
            distance,
            length,
            rep_index: 0,
        };
        self.count += 1;
    }
}

#[derive(Clone, Copy)]
struct MatchSearch {
    dict_size: usize,
    end: usize,
    nice: usize,
    position: usize,
}

struct MatchFinderBt4 {
    cyclic_size: usize,
    depth: u32,
    hash4_mask: usize,
    hash2: Vec<u32>,
    hash3: Vec<u32>,
    hash4: Vec<u32>,
    son: Vec<u32>,
}

impl MatchFinderBt4 {
    fn new(
        input_len: usize,
        depth: u32,
        mode: CompressionMode,
        dict_size: u32,
        nice: usize,
    ) -> MatchFinderBt4 {
        assert!(input_len <= u32::MAX as usize);

        let cyclic_size = input_len.min(dict_size as usize).saturating_add(1);
        let depth = if depth == 0 {
            if mode == CompressionMode::Fast {
                32
            } else {
                16 + nice as u32 / 2
            }
        } else {
            depth
        };
        let hash4_size = hash4_size(dict_size);

        MatchFinderBt4 {
            cyclic_size,
            depth,
            hash4_mask: hash4_size - 1,
            hash2: vec![EMPTY_MATCH; 1 << 16],
            hash3: vec![EMPTY_MATCH; 1 << 16],
            hash4: vec![EMPTY_MATCH; hash4_size],
            son: vec![EMPTY_MATCH; cyclic_size * 2],
        }
    }

    fn insert(&mut self, input: &[u8], position: usize) {
        let candidate = self.update_hashes(input, position);
        self.bt_insert(input, self.search(input, position), candidate, None, 3);
    }

    fn skip_insert(&mut self, input: &[u8], start: usize, end: usize) {
        for position in start..end {
            self.insert(input, position);
        }
    }

    fn find_matches(
        &mut self,
        input: &[u8],
        position: usize,
        end: usize,
        dict_size: usize,
        nice: usize,
        matches: &mut MatchList,
    ) {
        if position + MATCH_LEN_MIN > end {
            self.insert(input, position);
            return;
        }

        let search = MatchSearch {
            dict_size,
            end,
            nice,
            position,
        };
        let hash2_candidate = self.hash2[hash2(input, position)];
        let hash3_candidate = if position + 3 <= end {
            self.hash3[hash3(input, position)]
        } else {
            EMPTY_MATCH
        };

        let candidate = self.update_hashes(input, position);
        self.test_short_candidate(input, search, hash2_candidate, matches);
        self.test_short_candidate(input, search, hash3_candidate, matches);

        let best = matches.best().map_or(3, |best| best.length as usize).max(3);
        if best >= nice {
            self.bt_insert(input, search, candidate, None, best);
            self.extend_best_match(input, position, end, matches);
            return;
        }

        self.bt_insert(input, search, candidate, Some(matches), best);
    }

    fn peek_matches(
        &self,
        input: &[u8],
        position: usize,
        end: usize,
        dict_size: usize,
        nice: usize,
        matches: &mut MatchList,
    ) {
        if position + MATCH_LEN_MIN > end {
            return;
        }

        let search = MatchSearch {
            dict_size,
            end,
            nice,
            position,
        };
        let hash2_candidate = self.hash2[hash2(input, position)];
        let hash3_candidate = if position + 3 <= end {
            self.hash3[hash3(input, position)]
        } else {
            EMPTY_MATCH
        };
        let candidate = if position + 4 <= end {
            self.hash4[hash4(input, position, self.hash4_mask)]
        } else {
            EMPTY_MATCH
        };

        self.test_short_candidate(input, search, hash2_candidate, matches);
        self.test_short_candidate(input, search, hash3_candidate, matches);
        let best = matches.best().map_or(3, |best| best.length as usize).max(3);

        self.bt_peek(input, search, candidate, matches, best);
    }

    fn search(&self, input: &[u8], position: usize) -> MatchSearch {
        MatchSearch {
            dict_size: self.cyclic_size.saturating_sub(1),
            end: input.len(),
            nice: MATCH_LEN_MAX,
            position,
        }
    }

    fn update_hashes(&mut self, input: &[u8], position: usize) -> u32 {
        if position + 4 <= input.len() {
            let word =
                unsafe { std::ptr::read_unaligned(input.as_ptr().add(position).cast::<u32>()) };
            let word = u32::from_le(word);
            let byte0 = (word & 0xFF) as u8;
            let byte1 = ((word >> 8) & 0xFF) as u8;
            let byte2 = ((word >> 16) & 0xFF) as u8;
            let byte3 = (word >> 24) as u8;
            let hash2 = usize::from(byte0) | (usize::from(byte1) << 8);
            self.hash2[hash2] = position as u32;

            let temp = lz_hash_table(byte0) ^ u32::from(byte1);
            let hash23 = temp ^ (u32::from(byte2) << 8);
            let hash3 = (hash23 & 0xFFFF) as usize;
            self.hash3[hash3] = position as u32;

            let hash4 = (hash23 ^ (lz_hash_table(byte3) << 5)) as usize & self.hash4_mask;
            let candidate = self.hash4[hash4];
            self.hash4[hash4] = position as u32;

            return candidate;
        }

        if position + 2 <= input.len() {
            let byte0 = input[position];
            let byte1 = input[position + 1];
            let hash2 = usize::from(byte0) | (usize::from(byte1) << 8);
            self.hash2[hash2] = position as u32;

            let temp = lz_hash_table(byte0) ^ u32::from(byte1);
            if position + 3 <= input.len() {
                let byte2 = input[position + 2];
                let hash3 = ((temp ^ (u32::from(byte2) << 8)) & 0xFFFF) as usize;
                self.hash3[hash3] = position as u32;
            }
        }

        self.clear_current_son(position);
        EMPTY_MATCH
    }

    fn clear_current_son(&mut self, position: usize) {
        let pair = self.son_index(position);
        self.son[pair] = EMPTY_MATCH;
        self.son[pair + 1] = EMPTY_MATCH;
    }

    fn bt_insert(
        &mut self,
        input: &[u8],
        search: MatchSearch,
        mut candidate: u32,
        mut matches: Option<&mut MatchList>,
        mut best: usize,
    ) {
        let current_pair = self.son_index(search.position);
        let current_cyclic = current_pair / 2;
        let mut ptr0 = current_pair + 1;
        let mut ptr1 = current_pair;
        let mut len0 = 0usize;
        let mut len1 = 0usize;
        let emit_limit = match_limit(search.position, search.end, search.nice);
        let tree_limit = match_limit(search.position, input.len(), MATCH_LEN_MAX);
        let mut depth = self.depth;

        loop {
            if candidate == EMPTY_MATCH
                || depth == 0
                || !candidate_in_window(search.position, candidate as usize, search.dict_size)
            {
                self.son[ptr0] = EMPTY_MATCH;
                self.son[ptr1] = EMPTY_MATCH;
                return;
            }

            depth -= 1;
            let candidate_position = candidate as usize;
            let distance = search.position - candidate_position;
            let pair = self.son_index_at_distance(current_cyclic, distance);
            let mut length = len0.min(len1);

            // `length < tree_limit` bounds both probes; candidates are always
            // before the current position and inside the dictionary window.
            if length < tree_limit
                && unsafe {
                    *input.get_unchecked(candidate_position + length)
                        == *input.get_unchecked(search.position + length)
                }
            {
                length = match_length_from(
                    input,
                    search.position,
                    candidate_position,
                    input.len(),
                    MATCH_LEN_MAX,
                    length + 1,
                );

                let emit_length = length.min(emit_limit);
                if emit_length > best {
                    best = emit_length;
                    if let Some(matches) = matches.as_deref_mut() {
                        matches.push(
                            (search.position - candidate_position) as u32,
                            emit_length as u32,
                        );
                    }
                }

                if length == tree_limit {
                    self.son[ptr1] = self.son[pair];
                    self.son[ptr0] = self.son[pair + 1];
                    return;
                }
            }

            if length >= tree_limit {
                self.son[ptr1] = self.son[pair];
                self.son[ptr0] = self.son[pair + 1];
                return;
            }

            let candidate_byte = unsafe { *input.get_unchecked(candidate_position + length) };
            let current_byte = unsafe { *input.get_unchecked(search.position + length) };
            if candidate_byte < current_byte {
                self.son[ptr1] = candidate;
                ptr1 = pair + 1;
                candidate = self.son[ptr1];
                len1 = length;
            } else {
                self.son[ptr0] = candidate;
                ptr0 = pair;
                candidate = self.son[ptr0];
                len0 = length;
            }
        }
    }

    fn bt_peek(
        &self,
        input: &[u8],
        search: MatchSearch,
        mut candidate: u32,
        matches: &mut MatchList,
        mut best: usize,
    ) {
        let current_cyclic = self.son_index(search.position) / 2;
        let mut len0 = 0usize;
        let mut len1 = 0usize;
        let emit_limit = match_limit(search.position, search.end, search.nice);
        let tree_limit = match_limit(search.position, input.len(), MATCH_LEN_MAX);
        let mut depth = self.depth;

        while candidate != EMPTY_MATCH
            && depth > 0
            && candidate_in_window(search.position, candidate as usize, search.dict_size)
        {
            depth -= 1;
            let candidate_position = candidate as usize;
            let distance = search.position - candidate_position;
            let pair = self.son_index_at_distance(current_cyclic, distance);
            let mut length = len0.min(len1);

            // `length < tree_limit` bounds both probes; candidates are always
            // before the current position and inside the dictionary window.
            if length < tree_limit
                && unsafe {
                    *input.get_unchecked(candidate_position + length)
                        == *input.get_unchecked(search.position + length)
                }
            {
                length = match_length_from(
                    input,
                    search.position,
                    candidate_position,
                    input.len(),
                    MATCH_LEN_MAX,
                    length + 1,
                );

                let emit_length = length.min(emit_limit);
                if emit_length > best {
                    best = emit_length;
                    matches.push(
                        (search.position - candidate_position) as u32,
                        emit_length as u32,
                    );
                }

                if length == tree_limit {
                    return;
                }
            }

            if length >= tree_limit {
                return;
            }

            let candidate_byte = unsafe { *input.get_unchecked(candidate_position + length) };
            let current_byte = unsafe { *input.get_unchecked(search.position + length) };
            if candidate_byte < current_byte {
                candidate = self.son[pair + 1];
                len1 = length;
            } else {
                candidate = self.son[pair];
                len0 = length;
            }
        }
    }

    fn son_index(&self, position: usize) -> usize {
        (position % self.cyclic_size) * 2
    }

    fn son_index_at_distance(&self, current_cyclic: usize, distance: usize) -> usize {
        debug_assert!(distance < self.cyclic_size);

        let cyclic = if current_cyclic >= distance {
            current_cyclic - distance
        } else {
            current_cyclic + self.cyclic_size - distance
        };

        cyclic * 2
    }

    fn test_short_candidate(
        &self,
        input: &[u8],
        search: MatchSearch,
        candidate: u32,
        matches: &mut MatchList,
    ) {
        if !candidate_in_window(search.position, candidate as usize, search.dict_size) {
            return;
        }

        let candidate_position = candidate as usize;
        let length = match_length(
            input,
            search.position,
            candidate_position,
            search.end,
            search.nice,
        );
        if length >= MATCH_LEN_MIN {
            matches.push((search.position - candidate_position) as u32, length as u32);
        }
    }

    fn extend_best_match(
        &self,
        input: &[u8],
        position: usize,
        end: usize,
        matches: &mut MatchList,
    ) {
        let Some(best) = matches.best() else {
            return;
        };

        let candidate = position - best.distance as usize;
        let length = match_length(input, position, candidate, end, MATCH_LEN_MAX);
        if length > best.length as usize {
            matches.push(best.distance, length as u32);
        }
    }
}

fn choose_decision(normal: Option<MatchCandidate>, reps: &[MatchCandidate; 4]) -> ParseDecision {
    let rep = best_rep(reps);

    let normal_is_worthwhile = normal.is_some_and(normal_match_is_worthwhile);

    if rep.rep_index == 0 && rep.length == 1 && !normal_is_worthwhile {
        return rep_decision(rep, 1);
    }

    if rep.length >= MATCH_LEN_MIN as u32 {
        if let Some(normal) = normal {
            if !normal_match_is_worthwhile(normal)
                || rep.length + rep_margin(normal.distance) >= normal.length
            {
                return rep_decision(rep, rep.length);
            }
        } else {
            return rep_decision(rep, rep.length);
        }
    }

    if let Some(normal) = normal
        && normal_is_worthwhile
    {
        return ParseDecision {
            distance: normal.distance,
            kind: DecisionKind::Match,
            length: normal.length,
            rep_index: 0,
        };
    }

    ParseDecision::literal()
}

fn adjusted_normal_candidate(matches: &MatchList) -> Option<MatchCandidate> {
    let mut index = matches.count.checked_sub(1)?;
    let mut candidate = matches.matches[index];

    while index > 0 {
        let previous = matches.matches[index - 1];
        if candidate.length != previous.length + 1 {
            break;
        }

        if !change_pair(
            zero_based_distance(previous.distance),
            zero_based_distance(candidate.distance),
        ) {
            break;
        }

        candidate = previous;
        index -= 1;
    }

    if normal_match_is_worthwhile(candidate) {
        Some(candidate)
    } else {
        None
    }
}

fn best_rep(reps: &[MatchCandidate; 4]) -> MatchCandidate {
    let mut best = MatchCandidate::empty();

    for &candidate in reps {
        if candidate.length > best.length {
            best = candidate;
        }
    }

    best
}

fn rep_decision(rep: MatchCandidate, length: u32) -> ParseDecision {
    ParseDecision {
        distance: rep.distance,
        kind: DecisionKind::Rep,
        length,
        rep_index: rep.rep_index,
    }
}

fn edge_total_length(edge: &[ParseDecision]) -> u32 {
    edge.iter().map(|decision| decision.length).sum()
}

fn normal_match_is_worthwhile(normal: MatchCandidate) -> bool {
    if normal.length >= 5 {
        return true;
    }

    if normal.length == 4 {
        return normal.distance <= 131_072;
    }

    if normal.length == 3 {
        return normal.distance <= 1024;
    }

    normal.length == 2 && normal.distance <= 16
}

fn lazy_next_normal_beats_current(current: ParseDecision, next: MatchCandidate) -> bool {
    let current_distance = zero_based_distance(current.distance);
    let next_distance = zero_based_distance(next.distance);

    (next.length >= current.length && next_distance < current_distance)
        || (next.length == current.length + 1 && !change_pair(current_distance, next_distance))
        || next.length > current.length + 1
        || (next.length + 1 >= current.length
            && current.length >= 3
            && change_pair(next_distance, current_distance))
}

fn change_pair(small_distance: u32, big_distance: u32) -> bool {
    (big_distance >> 7) > small_distance
}

fn zero_based_distance(distance: u32) -> u32 {
    distance.saturating_sub(1)
}

fn rep_margin(distance: u32) -> u32 {
    let distance = zero_based_distance(distance);

    if distance > (1 << 15) {
        4
    } else if distance > (1 << 9) {
        3
    } else {
        1
    }
}

fn better_normal(
    first: Option<MatchCandidate>,
    second: Option<MatchCandidate>,
) -> Option<MatchCandidate> {
    match (first, second) {
        (Some(first), Some(second)) => {
            if normal_candidate_is_better(second.length, second.distance as usize, first) {
                Some(second)
            } else {
                Some(first)
            }
        }
        (Some(candidate), None) | (None, Some(candidate)) => Some(candidate),
        (None, None) => None,
    }
}

fn synthetic_next_match(
    input: &[u8],
    position: usize,
    candidate: usize,
    end: usize,
    nice: usize,
) -> Option<MatchCandidate> {
    if position <= candidate || position + MATCH_LEN_MIN > end {
        return None;
    }

    let length = match_length(input, position, candidate, end, nice);
    if length < MATCH_LEN_MIN {
        return None;
    }

    Some(MatchCandidate {
        distance: (position - candidate) as u32,
        length: length as u32,
        rep_index: 0,
    })
}

fn normal_candidate_is_better(length: u32, distance: usize, best: MatchCandidate) -> bool {
    if length > best.length {
        return true;
    }

    length == best.length && best.length > 0 && distance < best.distance as usize
}

#[inline(always)]
fn match_length(input: &[u8], position: usize, candidate: usize, end: usize, nice: usize) -> usize {
    match_length_from(input, position, candidate, end, nice, 0)
}

#[inline(always)]
fn match_length_from(
    input: &[u8],
    position: usize,
    candidate: usize,
    end: usize,
    nice: usize,
    start: usize,
) -> usize {
    let limit = match_limit(position, end, nice);
    let mut length = start;

    if length + 8 <= limit {
        let difference = read_u64_unaligned(input, position + length)
            ^ read_u64_unaligned(input, candidate + length);
        if difference != 0 {
            return length + first_mismatch_u64(difference);
        }

        length += 8;
    } else if length + 4 <= limit {
        let difference = read_u32_unaligned(input, position + length)
            ^ read_u32_unaligned(input, candidate + length);
        if difference != 0 {
            return length + first_mismatch_u32(difference);
        }

        length += 4;
    }

    #[cfg(target_arch = "x86_64")]
    {
        // x86_64 guarantees SSE2, and these probes are already bounded by
        // `limit`, so unaligned vector loads are valid here.
        length = unsafe { match_length_from_sse2(input, position, candidate, limit, length) };
    }

    while length + 8 <= limit {
        let difference = read_u64_unaligned(input, position + length)
            ^ read_u64_unaligned(input, candidate + length);
        if difference != 0 {
            return length + first_mismatch_u64(difference);
        }

        length += 8;
    }

    while length < limit && input[position + length] == input[candidate + length] {
        length += 1;
    }

    length
}

#[inline(always)]
fn read_u32_unaligned(input: &[u8], offset: usize) -> u32 {
    debug_assert!(offset + 4 <= input.len());

    // The match finder probes arbitrary byte offsets, so aligned loads cannot
    // be assumed. Bounds are established by match_length_from before each call.
    unsafe { std::ptr::read_unaligned(input.as_ptr().add(offset).cast::<u32>()) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn match_length_from_sse2(
    input: &[u8],
    position: usize,
    candidate: usize,
    limit: usize,
    mut length: usize,
) -> usize {
    use core::arch::x86_64::{__m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8};

    while length + 16 <= limit {
        let current =
            unsafe { _mm_loadu_si128(input.as_ptr().add(position + length).cast::<__m128i>()) };
        let previous =
            unsafe { _mm_loadu_si128(input.as_ptr().add(candidate + length).cast::<__m128i>()) };
        let equal = _mm_movemask_epi8(_mm_cmpeq_epi8(current, previous)) as u32;
        let different = !equal & 0xFFFF;
        if different != 0 {
            return length + different.trailing_zeros() as usize;
        }

        length += 16;
    }

    length
}

#[inline(always)]
fn read_u64_unaligned(input: &[u8], offset: usize) -> u64 {
    debug_assert!(offset + 8 <= input.len());

    // The match finder probes arbitrary byte offsets, so aligned loads cannot
    // be assumed. Bounds are established by match_length_from before each call.
    unsafe { std::ptr::read_unaligned(input.as_ptr().add(offset).cast::<u64>()) }
}

#[inline(always)]
fn first_mismatch_u32(difference: u32) -> usize {
    debug_assert_ne!(difference, 0);

    #[cfg(target_endian = "little")]
    {
        difference.trailing_zeros() as usize / 8
    }

    #[cfg(target_endian = "big")]
    {
        difference.leading_zeros() as usize / 8
    }
}

#[inline(always)]
fn first_mismatch_u64(difference: u64) -> usize {
    debug_assert_ne!(difference, 0);

    #[cfg(target_endian = "little")]
    {
        difference.trailing_zeros() as usize / 8
    }

    #[cfg(target_endian = "big")]
    {
        difference.leading_zeros() as usize / 8
    }
}

fn match_limit(position: usize, end: usize, nice: usize) -> usize {
    (end - position).min(MATCH_LEN_MAX).min(nice)
}

fn candidate_in_window(position: usize, candidate: usize, dict_size: usize) -> bool {
    candidate < position && position - candidate <= dict_size
}

fn hash2(input: &[u8], position: usize) -> usize {
    usize::from(input[position]) | (usize::from(input[position + 1]) << 8)
}

fn hash3(input: &[u8], position: usize) -> usize {
    let temp = lz_hash_table(input[position]) ^ u32::from(input[position + 1]);

    ((temp ^ (u32::from(input[position + 2]) << 8)) & 0xFFFF) as usize
}

fn hash4(input: &[u8], position: usize, mask: usize) -> usize {
    let temp = lz_hash_table(input[position]) ^ u32::from(input[position + 1]);
    let value =
        temp ^ (u32::from(input[position + 2]) << 8) ^ (lz_hash_table(input[position + 3]) << 5);

    value as usize & mask
}

fn lz_hash_table(byte: u8) -> u32 {
    const TABLE: [u32; 256] = build_lz_hash_table();

    TABLE[byte as usize]
}

const fn build_lz_hash_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut byte = 0usize;

    while byte < table.len() {
        let mut value = byte as u32;
        let mut bit = 0;

        while bit < 8 {
            if value & 1 == 1 {
                value = (value >> 1) ^ 0xEDB8_8320;
            } else {
                value >>= 1;
            }

            bit += 1;
        }

        table[byte] = value;
        byte += 1;
    }

    table
}

fn hash4_size(dict_size: u32) -> usize {
    let mut mask = dict_size.saturating_sub(1);
    mask |= mask >> 1;
    mask |= mask >> 2;
    mask |= mask >> 4;
    mask |= mask >> 8;
    mask >>= 1;
    mask |= 0xFFFF;

    if mask > (1 << 24) {
        mask >>= 1;
    }

    (mask as usize + 1).max(1 << 16)
}

fn distance_to_pos_slot(distance: u32) -> u32 {
    if distance < START_POS_MODEL_INDEX {
        return distance;
    }

    let highest = 31 - distance.leading_zeros();

    (highest << 1) + ((distance >> (highest - 1)) & 1)
}

fn dist_state(length: u32) -> usize {
    ((length - MATCH_LEN_MIN as u32).min((NUM_LEN_TO_POS_STATES - 1) as u32)) as usize
}

fn dist_table_size(dict_size: u32) -> usize {
    let mut log_size = 0u32;

    while log_size < NUM_POS_SLOT_BITS && (1u32 << log_size) < dict_size {
        log_size += 1;
    }

    (log_size * 2).min(1 << NUM_POS_SLOT_BITS) as usize
}

fn advance_decision_state(
    state: u32,
    mut reps: [u32; 4],
    decision: ParseDecision,
) -> (u32, [u32; 4]) {
    match decision.kind {
        DecisionKind::Literal => (state_update_literal(state), reps),
        DecisionKind::Match => {
            reps[3] = reps[2];
            reps[2] = reps[1];
            reps[1] = reps[0];
            reps[0] = decision.distance - 1;

            (state_update_match(state), reps)
        }
        DecisionKind::Rep => {
            if decision.rep_index > 0 {
                let distance = reps[decision.rep_index as usize];
                let mut index = decision.rep_index as usize;

                while index > 0 {
                    reps[index] = reps[index - 1];
                    index -= 1;
                }

                reps[0] = distance;
            }

            if decision.length == 1 {
                (state_update_short_rep(state), reps)
            } else {
                (state_update_repetition(state), reps)
            }
        }
    }
}

fn fill_probs(probs: &mut [u16]) {
    for prob in probs {
        *prob = (BIT_MODEL_TOTAL / 2) as u16;
    }
}

fn state_is_literal(state: u32) -> bool {
    state < 7
}

fn state_update_literal(state: u32) -> u32 {
    if state < 4 {
        0
    } else if state < 10 {
        state - 3
    } else {
        state - 6
    }
}

fn state_update_match(state: u32) -> u32 {
    if state < 7 { 7 } else { 10 }
}

fn state_update_repetition(state: u32) -> u32 {
    if state < 7 { 8 } else { 11 }
}

fn state_update_short_rep(state: u32) -> u32 {
    if state < 7 { 9 } else { 11 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paired_input(len: usize) -> Vec<u8> {
        let mut input = vec![0; len * 2];
        for index in 0..len {
            input[index] = ((index * 37 + 11) & 0xFF) as u8;
            input[len + index] = input[index];
        }

        input
    }

    #[test]
    fn match_length_stops_at_first_vector_mismatch() {
        let mut input = paired_input(96);
        input[96 + 37] ^= 0x40;

        assert_eq!(match_length(&input, 96, 0, input.len(), MATCH_LEN_MAX), 37);
        assert_eq!(
            match_length_from(&input, 96, 0, input.len(), MATCH_LEN_MAX, 17),
            37
        );
    }

    #[test]
    fn match_length_honors_nice_limit() {
        let input = paired_input(96);

        assert_eq!(match_length(&input, 96, 0, input.len(), 41), 41);
    }

    #[test]
    fn match_length_handles_tail_mismatch() {
        let mut input = paired_input(24);
        input[24 + 21] ^= 0x01;

        assert_eq!(match_length(&input, 24, 0, input.len(), MATCH_LEN_MAX), 21);
    }
}
