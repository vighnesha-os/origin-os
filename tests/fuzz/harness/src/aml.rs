//! The AML interpreter (`origin-aml`; ACPI 6.6 chapter 20; ADR-0019 T1) on fuzzed definition
//! blocks, through a host of the harness's own.
//!
//! [`FuzzHost`] answers every request from a list of bytes the input carries: region reads,
//! bus numbers, buffer transfers, the Global Lock, tables for `Load` and `LoadTable`, and
//! failures of each. Its clock advances a fixed step per reading, and by what `Sleep` and
//! `Stall` ask, so a run depends on its input alone and can be replayed.
//!
//! [`definition_block`] writes a term list from the fuzzer's choices with the encodings of
//! ACPI 6.6 §20.2 (package lengths, name strings, the named objects, statements and
//! expressions), so that a run gets past the parser into the namespace and the evaluator.
//! [`exercise`] loads a block under `Limits::BOOT_PROOF`, the limits the boot runs with
//! (the coordinating session's ruling Q7), walks the namespace, evaluates objects, and runs
//! the device initialization walk and `_REG`.

use arbitrary::{Result, Unstructured};
use origin_aml::{
    BridgeBus, BufferAccess, Config, DefinitionBlock, Host, HostError, IntegerWidth, Interpreter,
    Limits, Node, RegionAccess, Report, Space, TableKey, Trace, Value,
};
use std::hint::black_box;

/// The most nodes [`exercise`] reads back from the namespace.
const WALK: usize = 256;
/// The most objects [`exercise`] evaluates after the load (each under the limits).
const EVALUATIONS: usize = 2;
/// The ACPI object type of a method (ACPI 6.6 §19.6.96, `ObjectType`).
const METHOD: u64 = 8;

/// A host whose every answer comes from `answers`, then zeros once they run out.
#[derive(Debug)]
pub struct FuzzHost {
    answers: Vec<u8>,
    at: usize,
    clock: u64,
    trace: bool,
}

impl FuzzHost {
    /// A host answering from `answers`. Its first byte's low bit turns tracing on.
    pub fn new(answers: &[u8]) -> Self {
        Self {
            trace: answers.first().is_some_and(|b| b & 1 == 1),
            answers: answers.to_vec(),
            at: 1,
            clock: 0,
        }
    }

    fn byte(&mut self) -> u8 {
        let b = self.answers.get(self.at).copied().unwrap_or(0);
        self.at = self.at.saturating_add(1);
        b
    }

    fn word(&mut self) -> u64 {
        let mut v = [0u8; 8];
        for b in &mut v {
            *b = self.byte();
        }
        u64::from_le_bytes(v)
    }

    /// Whether a request succeeds: most do, and each kind of failure now and then.
    fn outcome(&mut self) -> std::result::Result<(), HostError> {
        match self.byte() {
            0..=207 => Ok(()),
            208..=223 => Err(HostError::Refused),
            224..=239 => Err(HostError::Absent),
            240..=247 => Err(HostError::Timeout),
            _ => Err(HostError::Failed),
        }
    }
}

impl Host for FuzzHost {
    fn now_100ns(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(10);
        self.clock
    }

    fn read(&mut self, access: &RegionAccess<'_>) -> std::result::Result<u64, HostError> {
        black_box(access);
        self.outcome()?;
        let v = self.word();
        Ok(if access.width >= 64 {
            v
        } else {
            v & ((1u64 << access.width) - 1)
        })
    }

    fn write(
        &mut self,
        access: &RegionAccess<'_>,
        value: u64,
    ) -> std::result::Result<(), HostError> {
        black_box((access, value));
        self.outcome()
    }

    fn bridge_bus(&mut self, request: &BridgeBus) -> std::result::Result<u8, HostError> {
        black_box(request);
        self.outcome()?;
        Ok(self.byte())
    }

    fn transfer(
        &mut self,
        access: &BufferAccess<'_>,
        data: &mut [u8],
    ) -> std::result::Result<usize, HostError> {
        black_box(access);
        self.outcome()?;
        for d in data.iter_mut() {
            *d = self.byte();
        }
        Ok(data.len())
    }

    fn sleep_ms(&mut self, ms: u64) {
        self.clock = self.clock.saturating_add(ms.saturating_mul(10_000));
    }

    fn stall_us(&mut self, us: u8) {
        self.clock = self.clock.saturating_add(u64::from(us) * 10);
    }

    fn acquire_global_lock(&mut self, timeout_ms: u16) -> std::result::Result<bool, HostError> {
        black_box(timeout_ms);
        self.outcome()?;
        Ok(self.byte() & 1 == 0)
    }

    fn release_global_lock(&mut self) -> std::result::Result<(), HostError> {
        self.outcome()
    }

    fn notify(&mut self, node: &[[u8; 4]], value: u64) {
        black_box((node, value));
    }

    fn fatal(&mut self, kind: u8, code: u32, arg: u64) {
        black_box((kind, code, arg));
    }

    fn debug(&mut self, text: &[u8]) {
        black_box(text);
    }

    fn report(&mut self, report: &Report<'_>) {
        black_box(report);
    }

    fn tracing(&self) -> bool {
        self.trace
    }

    fn trace(&mut self, event: &Trace<'_>) {
        black_box(event);
    }

    /// Now and then a table for `Load` or `LoadTable`: an ACPI header with the key's signature
    /// and IDs, its length and checksum right, and an AML body from the answers.
    fn table(
        &mut self,
        key: &TableKey,
        out: &mut dyn FnMut(u64, &[u8]),
    ) -> std::result::Result<bool, HostError> {
        self.outcome()?;
        if self.byte() & 3 != 0 {
            return Ok(false);
        }
        let body = usize::from(self.byte());
        let mut t = Vec::with_capacity(36 + body);
        t.extend_from_slice(&key.signature);
        t.extend_from_slice(&u32::try_from(36 + body).unwrap_or(0).to_le_bytes());
        t.push(2);
        t.push(0);
        t.extend_from_slice(&key.oem_id);
        t.extend_from_slice(&key.oem_table_id);
        t.extend_from_slice(&[1, 0, 0, 0]);
        t.extend_from_slice(b"ORGN");
        t.extend_from_slice(&[1, 0, 0, 0]);
        for _ in 0..body {
            t.push(self.byte());
        }
        let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        t[9] = 0u8.wrapping_sub(sum);
        out(self.word() & 0x0000_FFFF_FFFF_F000, &t);
        Ok(true)
    }
}

/// Loads `aml` as a DSDT under the boot's limits with `answers` as the host's, reads back the
/// namespace, evaluates a few of its objects, and runs the initialization walk and `_REG`.
/// Nothing is judged here but that no call panics: the interpreter's own rule is that every
/// failure is an error (ADR-0019 A3), and libFuzzer's time and memory limits catch a run that
/// the interpreter's limits do not bound.
pub fn exercise(aml: &[u8], answers: &[u8], width: IntegerWidth) {
    let config = Config {
        limits: Limits::BOOT_PROOF,
    };
    let Ok(mut i) = Interpreter::new(FuzzHost::new(answers), config, width) else {
        return;
    };
    let block = DefinitionBlock {
        signature: *b"DSDT",
        oem_id: *b"ORIGIN",
        oem_table_id: *b"FUZZ    ",
        aml,
    };
    let _ = i.load(block);
    let nodes = walk(&i);
    let mut methods = Vec::new();
    let mut others = Vec::new();
    for &node in &nodes {
        let _ = black_box((i.name_of(node), i.is_loaded(node), i.path_of(node)));
        let _ = i.plain(node).map(|p| black_box(p.is_some()));
        match i.object_type(node) {
            Ok(METHOD) => methods.push(node),
            Ok(_) => others.push(node),
            Err(_) => {}
        }
    }
    let args = [
        Value::Integer(answers.get(1).copied().map_or(0, u64::from)),
        Value::Buffer(answers.get(..4).unwrap_or_default().to_vec()),
    ];
    for node in methods
        .iter()
        .rev()
        .chain(others.iter().rev())
        .take(EVALUATIONS)
    {
        let _ = i.evaluate_node(*node, &args);
    }
    let _ = i.initialize(&mut |node, outcome| {
        black_box((node, outcome));
    });
    let _ = i.connect_space(Space::EMBEDDED_CONTROL, true, &mut |node, result| {
        black_box((node, result.is_ok()));
    });
}

/// The namespace from the root, depth first, at most [`WALK`] nodes.
fn walk<H: Host>(i: &Interpreter<H>) -> Vec<Node> {
    let mut out = Vec::new();
    let mut stack = vec![i.root()];
    while let Some(n) = stack.pop() {
        if out.len() >= WALK {
            break;
        }
        out.push(n);
        let mut child = i.first_child(n);
        while let Some(c) = child {
            if stack.len() >= WALK {
                break;
            }
            stack.push(c);
            child = i.next_sibling(c);
        }
    }
    out
}

/// The names the writer defines and refers to: a small set, so that names meet again.
const NAMES: [[u8; 4]; 20] = [
    *b"DEV0", *b"DEV1", *b"M000", *b"M001", *b"M002", *b"NAM0", *b"NAM1", *b"BUF0", *b"PKG0",
    *b"OPR0", *b"FLD0", *b"FLD1", *b"MTX0", *b"EVT0", *b"_STA", *b"_INI", *b"_REG", *b"_HID",
    *b"_ADR", *b"_SB_",
];

/// What the writer knows of what it has defined: methods by absolute path, with their
/// argument counts, so that an invocation it writes has the right number of arguments.
struct Writer<'a, 'u> {
    u: &'u mut Unstructured<'a>,
    scope: Vec<[u8; 4]>,
    methods: Vec<(Vec<[u8; 4]>, u8)>,
    budget: u32,
}

/// Writes a DSDT's AML (ACPI 6.6 §20.2.1, `TermList`) from `u`'s choices.
pub fn definition_block(u: &mut Unstructured<'_>) -> Result<Vec<u8>> {
    let mut w = Writer {
        u,
        scope: Vec::new(),
        methods: Vec::new(),
        budget: 160,
    };
    let mut out = Vec::new();
    w.term_list(&mut out, 0)?;
    Ok(out)
}

/// Appends a package length (§20.2.4) for `body`, counting the length's own bytes, then `body`.
fn pkg(out: &mut Vec<u8>, body: &[u8]) {
    let n = body.len();
    if n + 1 < 0x40 {
        out.push(u8::try_from(n + 1).unwrap_or(0));
    } else {
        let (extra, total) = if n + 2 < 0x1000 {
            (1, n + 2)
        } else if n + 3 < 0x10_0000 {
            (2, n + 3)
        } else {
            (3, n + 4)
        };
        raw_length(out, extra, total);
    }
    out.extend_from_slice(body);
}

/// A package-length encoding of `value` with `extra` following bytes (1 to 3).
fn raw_length(out: &mut Vec<u8>, extra: u8, value: usize) {
    out.push((extra << 6) | u8::try_from(value & 0xF).unwrap_or(0));
    for k in 0..usize::from(extra) {
        out.push(u8::try_from((value >> (4 + 8 * k)) & 0xFF).unwrap_or(0));
    }
}

/// A field's width in bits, in the package-length encoding without counting itself (§20.2.5.2).
fn field_length(out: &mut Vec<u8>, bits: usize) {
    if bits < 0x40 {
        out.push(u8::try_from(bits).unwrap_or(0));
    } else {
        raw_length(out, 1, bits);
    }
}

impl Writer<'_, '_> {
    fn spend(&mut self) -> bool {
        if self.budget == 0 || self.u.is_empty() {
            return false;
        }
        self.budget -= 1;
        true
    }

    fn name_seg(&mut self) -> Result<[u8; 4]> {
        if self.u.ratio(15, 16)? {
            Ok(*self.u.choose(&NAMES)?)
        } else {
            let mut s = [b'_'; 4];
            for (k, b) in s.iter_mut().enumerate() {
                let c: u8 = self.u.int_in_range(0..=37)?;
                *b = match c {
                    0..=25 => b'A' + c,
                    26..=35 if k > 0 => b'0' + c - 26,
                    _ => b'_',
                };
            }
            Ok(s)
        }
    }

    /// A name string (§20.2.2): a name, now and then from the root, a parent, two or three
    /// segments, or the null name.
    fn name_string(&mut self, out: &mut Vec<u8>) -> Result<()> {
        match self.u.int_in_range(0..=15)? {
            0 => out.push(b'\\'),
            1 => out.push(b'^'),
            _ => {}
        }
        match self.u.int_in_range(0..=15)? {
            0 => out.push(0x00),
            1 => {
                out.push(0x2E);
                for _ in 0..2 {
                    out.extend_from_slice(&self.name_seg()?);
                }
            }
            2 => {
                let n = self.u.int_in_range(3..=4u8)?;
                out.push(0x2F);
                out.push(n);
                for _ in 0..n {
                    out.extend_from_slice(&self.name_seg()?);
                }
            }
            _ => out.extend_from_slice(&self.name_seg()?),
        }
        Ok(())
    }

    /// A list of terms (§20.2.5).
    fn term_list(&mut self, out: &mut Vec<u8>, depth: u32) -> Result<()> {
        let n = self.u.int_in_range(0..=6)?;
        for _ in 0..n {
            if !self.spend() {
                break;
            }
            self.term(out, depth)?;
        }
        Ok(())
    }

    /// One term: a named object, a statement, or an expression whose value is dropped.
    fn term(&mut self, out: &mut Vec<u8>, depth: u32) -> Result<()> {
        let deep = depth >= 5;
        match self.u.int_in_range(0..=22)? {
            0 | 1 => {
                out.push(0x08);
                let seg = self.name_seg()?;
                out.extend_from_slice(&seg);
                self.data(out, depth)?;
            }
            2 if !deep => self.scoped(out, &[0x10], depth)?,
            3 | 4 if !deep => self.scoped(out, &[0x5B, 0x82], depth)?,
            5 | 6 if !deep => self.method(out, depth)?,
            7 => {
                // OperationRegion (§20.2.5.2) and a Field over it.
                let region = self.name_seg()?;
                out.extend_from_slice(&[0x5B, 0x80]);
                out.extend_from_slice(&region);
                out.push(self.u.int_in_range(0..=10)?);
                self.expr(out, depth + 1)?;
                self.expr(out, depth + 1)?;
                self.field(out, region)?;
            }
            8 => {
                let region = self.name_seg()?;
                self.field(out, region)?;
            }
            9 => {
                out.extend_from_slice(&[0x5B, 0x01]);
                self.name_string(out)?;
                out.push(self.u.int_in_range(0..=15)?);
            }
            10 => {
                out.extend_from_slice(&[0x5B, 0x02]);
                self.name_string(out)?;
            }
            11 if !deep => {
                // If, perhaps with Else (§20.2.5.3).
                let mut body = Vec::new();
                self.expr(&mut body, depth + 1)?;
                self.term_list(&mut body, depth + 1)?;
                out.push(0xA0);
                pkg(out, &body);
                if self.u.ratio(1, 2)? {
                    let mut other = Vec::new();
                    self.term_list(&mut other, depth + 1)?;
                    out.push(0xA1);
                    pkg(out, &other);
                }
            }
            12 if !deep => {
                let mut body = Vec::new();
                self.expr(&mut body, depth + 1)?;
                self.term_list(&mut body, depth + 1)?;
                if self.u.ratio(1, 2)? {
                    body.push(*self.u.choose(&[0xA5, 0x9F])?);
                }
                out.push(0xA2);
                pkg(out, &body);
            }
            13 => {
                out.push(0x70);
                self.expr(out, depth + 1)?;
                self.super_name(out)?;
            }
            14 => {
                out.push(0xA4);
                self.expr(out, depth + 1)?;
            }
            15 => {
                out.push(0x86);
                self.super_name(out)?;
                self.expr(out, depth + 1)?;
            }
            16 => {
                // Sleep, Stall, Acquire, Release, Signal, Wait, Reset.
                match self.u.int_in_range(0..=6)? {
                    0 => {
                        out.extend_from_slice(&[0x5B, 0x22]);
                        self.expr(out, depth + 1)?;
                    }
                    1 => {
                        out.extend_from_slice(&[0x5B, 0x21]);
                        self.expr(out, depth + 1)?;
                    }
                    2 => {
                        out.extend_from_slice(&[0x5B, 0x23]);
                        self.super_name(out)?;
                        let timeout: u16 = self.u.arbitrary()?;
                        out.extend_from_slice(&timeout.to_le_bytes());
                    }
                    3 => {
                        out.extend_from_slice(&[0x5B, 0x27]);
                        self.super_name(out)?;
                    }
                    4 => {
                        out.extend_from_slice(&[0x5B, 0x24]);
                        self.super_name(out)?;
                    }
                    5 => {
                        out.extend_from_slice(&[0x5B, 0x25]);
                        self.super_name(out)?;
                        self.expr(out, depth + 1)?;
                    }
                    _ => {
                        out.extend_from_slice(&[0x5B, 0x26]);
                        self.super_name(out)?;
                    }
                }
            }
            17 => {
                out.push(*self.u.choose(&[0x75, 0x76])?);
                self.super_name(out)?;
            }
            18 => {
                // CreateField and its fixed-width forms (§20.2.5.2).
                if self.u.ratio(1, 2)? {
                    out.push(*self.u.choose(&[0x8A, 0x8B, 0x8C, 0x8D, 0x8F])?);
                    self.expr(out, depth + 1)?;
                    self.expr(out, depth + 1)?;
                } else {
                    out.extend_from_slice(&[0x5B, 0x13]);
                    self.expr(out, depth + 1)?;
                    self.expr(out, depth + 1)?;
                    self.expr(out, depth + 1)?;
                }
                self.name_string(out)?;
            }
            19 => {
                // Alias, External, Fatal.
                match self.u.int_in_range(0..=2)? {
                    0 => {
                        out.push(0x06);
                        self.name_string(out)?;
                        self.name_string(out)?;
                    }
                    1 => {
                        out.push(0x15);
                        self.name_string(out)?;
                        out.push(self.u.int_in_range(0..=16)?);
                        out.push(self.u.int_in_range(0..=7)?);
                    }
                    _ => {
                        out.extend_from_slice(&[0x5B, 0x32]);
                        out.push(self.u.arbitrary()?);
                        let code: u32 = self.u.arbitrary()?;
                        out.extend_from_slice(&code.to_le_bytes());
                        self.expr(out, depth + 1)?;
                    }
                }
            }
            20 => {
                // LoadTable: six arguments, the host's tables behind it.
                out.extend_from_slice(&[0x5B, 0x1F]);
                for s in ["SSDT", "ORIGIN", "FUZZ    ", "", "", ""] {
                    self.string(out, s.as_bytes());
                }
                self.super_name(out)?;
            }
            21 => {
                // A raw opcode byte, so that what the writer never names is reached too.
                out.push(self.u.arbitrary()?);
            }
            _ => self.expr(out, depth + 1)?,
        }
        Ok(())
    }

    /// Scope or Device: a package length, a name, and a term list inside it.
    fn scoped(&mut self, out: &mut Vec<u8>, op: &[u8], depth: u32) -> Result<()> {
        let seg = self.name_seg()?;
        let mut body = Vec::new();
        body.extend_from_slice(&seg);
        self.scope.push(seg);
        let r = self.term_list(&mut body, depth + 1);
        self.scope.pop();
        r?;
        out.extend_from_slice(op);
        pkg(out, &body);
        Ok(())
    }

    /// A method (§20.2.5.2): its flags, then its body. The writer records its absolute path and
    /// argument count, for invocations written after it.
    fn method(&mut self, out: &mut Vec<u8>, depth: u32) -> Result<()> {
        let seg = self.name_seg()?;
        let args = self.u.int_in_range(0..=7u8)?;
        let flags =
            args | (u8::from(self.u.ratio(1, 4)?) << 3) | (self.u.int_in_range(0..=15u8)? << 4);
        let mut path = self.scope.clone();
        path.push(seg);
        let mut body = Vec::new();
        body.extend_from_slice(&seg);
        body.push(flags);
        self.scope.push(seg);
        let r = self.term_list(&mut body, depth + 1);
        self.scope.pop();
        r?;
        self.methods.push((path, args));
        out.push(0x14);
        pkg(out, &body);
        Ok(())
    }

    /// A Field over `region` (§20.2.5.2): flags, then named, reserved and access entries.
    fn field(&mut self, out: &mut Vec<u8>, region: [u8; 4]) -> Result<()> {
        let mut body = Vec::new();
        body.extend_from_slice(&region);
        body.push(self.u.arbitrary()?);
        for _ in 0..self.u.int_in_range(1..=5)? {
            match self.u.int_in_range(0..=5)? {
                0 => {
                    body.push(0x00);
                    field_length(&mut body, self.u.int_in_range(1..=128)?);
                }
                1 => {
                    body.push(0x01);
                    body.push(self.u.int_in_range(0..=5)?);
                    body.push(self.u.arbitrary()?);
                }
                _ => {
                    let seg = self.name_seg()?;
                    body.extend_from_slice(&seg);
                    field_length(
                        &mut body,
                        *self.u.choose(&[1, 2, 4, 7, 8, 16, 32, 33, 64, 65, 200])?,
                    );
                }
            }
        }
        out.extend_from_slice(&[0x5B, 0x81]);
        pkg(out, &body);
        Ok(())
    }

    /// A string (§20.2.3): bytes, then a NUL.
    fn string(&mut self, out: &mut Vec<u8>, s: &[u8]) {
        out.push(0x0D);
        out.extend(s.iter().copied().filter(|&b| b != 0));
        out.push(0x00);
    }

    /// An integer in one of its encodings (§20.2.3).
    fn integer(&mut self, out: &mut Vec<u8>) -> Result<()> {
        match self.u.int_in_range(0..=6)? {
            0 => out.push(0x00),
            1 => out.push(0x01),
            2 => out.push(0xFF),
            3 => {
                out.push(0x0A);
                out.push(self.u.arbitrary()?);
            }
            4 => {
                out.push(0x0B);
                out.extend_from_slice(&self.u.arbitrary::<u16>()?.to_le_bytes());
            }
            5 => {
                out.push(0x0C);
                out.extend_from_slice(&self.u.arbitrary::<u32>()?.to_le_bytes());
            }
            _ => {
                out.push(0x0E);
                out.extend_from_slice(&self.u.arbitrary::<u64>()?.to_le_bytes());
            }
        }
        Ok(())
    }

    /// A data object: an integer, a string, a buffer or a package (§20.2.3).
    fn data(&mut self, out: &mut Vec<u8>, depth: u32) -> Result<()> {
        match self.u.int_in_range(0..=4)? {
            0 | 1 => self.integer(out)?,
            2 => {
                let n = self.u.int_in_range(0..=12)?;
                let s: Vec<u8> = (0..n)
                    .map(|_| self.u.int_in_range(0x20..=0x7Eu8))
                    .collect::<Result<_>>()?;
                self.string(out, &s);
            }
            3 => {
                let mut body = Vec::new();
                self.integer(&mut body)?;
                let n = self.u.int_in_range(0..=16)?;
                for _ in 0..n {
                    body.push(self.u.arbitrary()?);
                }
                out.push(0x11);
                pkg(out, &body);
            }
            _ => {
                let mut body = Vec::new();
                let n = if depth >= 5 {
                    0
                } else {
                    self.u.int_in_range(0..=5u8)?
                };
                body.push(n.saturating_add(self.u.int_in_range(0..=1u8)?));
                for _ in 0..n {
                    if self.u.ratio(1, 4)? {
                        self.name_string(&mut body)?;
                    } else {
                        self.data(&mut body, depth + 1)?;
                    }
                }
                out.push(if self.u.ratio(1, 8)? { 0x13 } else { 0x12 });
                if out.last() == Some(&0x13) {
                    let mut var = Vec::new();
                    self.integer(&mut var)?;
                    var.extend_from_slice(body.get(1..).unwrap_or_default());
                    pkg(out, &var);
                } else {
                    pkg(out, &body);
                }
            }
        }
        Ok(())
    }

    /// A super name (§20.2.6.2): a name, a local, an argument, Debug, or a reference.
    fn super_name(&mut self, out: &mut Vec<u8>) -> Result<()> {
        match self.u.int_in_range(0..=9)? {
            0..=2 => self.name_string(out)?,
            3..=5 => out.push(self.u.int_in_range(0x60..=0x67)?),
            6 | 7 => out.push(self.u.int_in_range(0x68..=0x6E)?),
            8 => out.extend_from_slice(&[0x5B, 0x31]),
            _ => {
                out.push(0x83);
                out.push(self.u.int_in_range(0x60..=0x67)?);
            }
        }
        Ok(())
    }

    /// A target: the null name, or a super name.
    fn target(&mut self, out: &mut Vec<u8>) -> Result<()> {
        if self.u.ratio(1, 2)? {
            out.push(0x00);
            Ok(())
        } else {
            self.super_name(out)
        }
    }

    /// An expression (§20.2.5.4): data, a local or argument, an operator over expressions, or
    /// a call of a method the writer defined before, with its number of arguments.
    fn expr(&mut self, out: &mut Vec<u8>, depth: u32) -> Result<()> {
        if depth >= 6 || !self.spend() {
            return self.integer(out);
        }
        match self.u.int_in_range(0..=16)? {
            0 | 1 => self.data(out, depth)?,
            2 => out.push(self.u.int_in_range(0x60..=0x67)?),
            3 => out.push(self.u.int_in_range(0x68..=0x6E)?),
            4 | 5 => {
                // Binary operators with a target.
                out.push(*self.u.choose(&[
                    0x72, 0x73, 0x74, 0x77, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F, 0x85,
                ])?);
                self.expr(out, depth + 1)?;
                self.expr(out, depth + 1)?;
                self.target(out)?;
            }
            6 => {
                out.push(0x78);
                self.expr(out, depth + 1)?;
                self.expr(out, depth + 1)?;
                self.target(out)?;
                self.target(out)?;
            }
            7 => {
                // Logical operators.
                let op = *self.u.choose(&[0x90, 0x91, 0x92, 0x93, 0x94, 0x95])?;
                out.push(op);
                self.expr(out, depth + 1)?;
                if op != 0x92 {
                    self.expr(out, depth + 1)?;
                }
            }
            8 => {
                // Conversions with a target.
                out.push(*self.u.choose(&[0x80, 0x81, 0x82, 0x96, 0x97, 0x98, 0x99])?);
                self.expr(out, depth + 1)?;
                self.target(out)?;
            }
            9 => {
                out.push(0x9C);
                self.expr(out, depth + 1)?;
                self.expr(out, depth + 1)?;
                self.target(out)?;
            }
            10 => {
                out.push(0x9E);
                self.expr(out, depth + 1)?;
                self.expr(out, depth + 1)?;
                self.expr(out, depth + 1)?;
                self.target(out)?;
            }
            11 => {
                out.push(0x88);
                self.expr(out, depth + 1)?;
                self.expr(out, depth + 1)?;
                self.target(out)?;
            }
            12 => {
                out.push(*self.u.choose(&[0x87, 0x8E, 0x71])?);
                self.super_name(out)?;
            }
            13 => {
                out.push(0x83);
                self.expr(out, depth + 1)?;
            }
            14 => {
                out.extend_from_slice(&[0x5B, 0x12]);
                self.super_name(out)?;
                self.target(out)?;
            }
            15 => out.extend_from_slice(&[0x5B, *self.u.choose(&[0x33, 0x30])?]),
            _ => {
                if self.methods.is_empty() {
                    self.integer(out)?;
                } else {
                    let i = self.u.choose_index(self.methods.len())?;
                    let (path, args) = self.methods[i].clone();
                    out.push(b'\\');
                    match path.len() {
                        1 => {}
                        2 => out.push(0x2E),
                        n => {
                            out.push(0x2F);
                            out.push(u8::try_from(n).unwrap_or(0));
                        }
                    }
                    for seg in &path {
                        out.extend_from_slice(seg);
                    }
                    for _ in 0..args {
                        self.expr(out, depth + 1)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// How many leading bytes of a raw input go to the host's answers.
pub const RAW_PREFIX: usize = 33;

/// The `aml` target: the first [`RAW_PREFIX`] bytes are the host's answers (the first byte's
/// second bit picks 32-bit integers), and the rest is the AML.
pub fn raw(data: &[u8]) {
    let (answers, aml) = data.split_at(data.len().min(RAW_PREFIX));
    exercise(aml, answers, width(answers));
}

/// The `aml_built` target: the host's answers, then a term list written from `data`'s choices.
pub fn built(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let written = (|| {
        let n = u.int_in_range(0..=512usize)?;
        let answers = u.bytes(n.min(u.len()))?.to_vec();
        Ok::<_, arbitrary::Error>((definition_block(&mut u)?, answers))
    })();
    if let Ok((aml, answers)) = written {
        exercise(&aml, &answers, width(&answers));
    }
}

/// 32-bit integers if the first answer byte's second bit is set (a DSDT below revision 2).
fn width(answers: &[u8]) -> IntegerWidth {
    if answers.first().is_some_and(|b| b & 2 == 2) {
        IntegerWidth::Bits32
    } else {
        IntegerWidth::Bits64
    }
}
