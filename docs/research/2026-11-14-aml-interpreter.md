# Research ledger: the AML interpreter library (ADR-0019 A3 and A5; TASKS item 63)

| | |
|---|---|
| Date | 2026-11-14 (UTC) |
| For | TASKS item 63: area B of ADR-0019 A6, the AML interpreter as a host-tested `no_std` library (A3) with an original AML encoder for its tests (A5) |
| Requires | CLAUDE.md §3 (original source), §16 (research first), §17 (ACPI 6.6), §58 (threat model), §67 (power management rests on ACPI); ADR-0019 A2 to A5 and T1, T5 |
| Method | ACPI 6.6, the owner's copy, read as text extracted in memory with Swift PDFKit (the file is encrypted). Microsoft's published pages read in the built-in browser. QEMU's own ACPI tables read from a guest's memory through QEMU's monitor, never from QEMU's source. **No ACPICA, Linux or other operating-system or interpreter source was read** (CLAUDE.md §3, ADR-0019 A4): what is left open is listed in §9 as named gaps for a separate session. The specification's text is paraphrased and cited by section, table and PDF page, never reproduced (§17). |

PDF page numbers below are the page's index in the PDF file (the outline's numbering), not the printed folio; in chapters 19 and 20 the printed number is 71 lower (PDF page 1129 is printed page 1058).

## 1. Sources

| ID | Source | Revision and date read |
|---|---|---|
| S1 | UEFI Forum, *Advanced Configuration and Power Interface (ACPI) Specification*, Release 6.6, the owner's copy `ACPI_Spec_6.6.pdf` | 1,202 pages, 6,955,208 bytes, SHA-256 `8c7542dd4de974ae47bba71bb0336637fe1e3838daad7692370ab4cf218efd35`. Read on 2026-11-14: chapter 20 entire (PDF 1129–1148); §5.3 to §5.8 (PDF 362–430); §19.3.5 (PDF 1004–1013) and, in §19.6, every operator an interpreter executes (PDF 1022–1127, the resource-descriptor macros skipped: they compile to buffers); §5.2.10.1 (PDF 200–201); §6.3.7 (PDF 497); §6.5.1 and §6.5.4 (PDF 547–551) |
| S2 | ACPI 6.6 errata, uefi.org | **Not read.** On 2026-11-14 at about 02:05 UTC `https://uefi.org/specifications` answered 403 with a "performing security verification" page in the built-in browser. A bot check is not something this session may pass, so no errata list was seen. See question Q1 |
| M1 | Microsoft Learn, *How to identify the Windows version in ACPI by using _OSI* (`learn.microsoft.com/en-us/windows-hardware/drivers/acpi/winacpi-osi`) | Page dated "Last updated 09/11/2024"; read 2026-11-14 |
| M2 | Microsoft Learn, *Overview of Windows Support for ACPI 5.0* (`learn.microsoft.com/en-us/windows-hardware/drivers/bringup/overview-of-windows-support-for-acpi-5-0`) | `ms.date` 2023-03-23, git commit `e5f48be3`; read 2026-11-14 |
| Q | QEMU 11.1.3 (the host's `qemu-system-x86_64 --version`) with `edk2-x86_64-code.fd` from the same installation: the tables OVMF installed, saved from guest memory with the monitor's `pmemsave` 25 seconds after power-on and walked from the RSDP | Five machines, each with `boot-test`'s options (q35, `-cpu max`, 512 MiB in two NUMA nodes, NVMe, virtio-net, no ESP): see §8 |

## 2. The encoding (S1 chapter 20, PDF 1129–1148)

- **A definition block** is the 36-byte table header followed by a term list (§20.2.1, PDF 1130–1131). The header's revision byte matters to the interpreter: per §19.6.29 (PDF 1035) the **DSDT's** revision sets the integer width for every table: below 2, integers are 32 bits; 2 or more, 64 bits. `Ones` follows it (§19.6.99, PDF 1081).
- **Names** (§20.2.2, PDF 1131–1132): a segment is four bytes, the first `A`–`Z` or `_`, the rest also `0`–`9`. A path is an optional root prefix `\` (0x5C) or any number of parent prefixes `^` (0x5E), then one segment, a dual-name prefix 0x2E and two segments, a multi-name prefix 0x2F with a count byte (1 to 255) and that many segments, or the null name 0x00. `SimpleName` is a path, an argument or a local; `SuperName` adds the debug object and the reference-type opcodes; `Target` is a `SuperName` or the null name.
- **Data** (§20.2.3, PDF 1132–1133): byte, word, dword and qword prefixes 0x0A, 0x0B, 0x0C, 0x0E with little-endian data; strings 0x0D followed by characters 0x01–0x7F and a NUL; constants Zero 0x00, One 0x01, Ones 0xFF and Revision 0x5B 0x30; buffers, packages and variable packages are data objects too.
- **Package length** (§20.2.4, PDF 1133; also §5.4.1, PDF 366): one to four bytes; the top two bits of the first byte count the bytes that follow. One byte holds 0–63 in its low six bits; with following bytes, bits 4–5 of the first byte must be zero, its low nibble is the length's low four bits and each following byte the next eight. The largest length is 2^28 − 1. The length counts itself and everything up to the end of the construct, and a construct's length must lie wholly inside its container's (§5.4.1).
- **Term objects** (§20.2.5, PDF 1133–1142): namespace modifiers (Alias 0x06, Name 0x08, Scope 0x10), named objects (BankField, the five fixed `Create…Field` and `CreateField`, DataRegion, Device, Event, External, Field, IndexField, Method, Mutex, OperationRegion, PowerResource, ThermalZone), statements (Break, BreakPoint, Continue, Fatal, If/Else, Noop, Notify, Release, Reset, Return, Signal, Sleep, Stall, While) and expressions (the rest, including method invocation, which is a name followed by as many terms as the method takes arguments). Field lists hold named fields, reserved fields (0x00), access fields (0x01), connections (0x02) and extended access fields (0x03).
- **Table 20.2** (PDF 1144–1147) lists every byte value. Two-byte opcodes start with 0x5B. `LNotEqual`, `LLessEqual` and `LGreaterEqual` are `LNot` (0x92) followed by `LEqual`, `LGreater` and `LLess`.
- **Region spaces** (§20.2.5.2 and Table 5.221, PDF 1136–1137 and 371–372): 0 SystemMemory, 1 SystemIO, 2 PCI_Config, 3 EmbeddedControl, 4 SMBus, 5 SystemCMOS, 6 PciBarTarget, 7 IPMI, 8 GeneralPurposeIO, 9 GenericSerialBus, 0x0A PCC, 0x0B PlatformRtMechanism, 0x7F FFixedHW, 0x80–0xFF OEM; the rest reserved.
- **Field flags** (PDF 1134): access type in bits 0–3 (Any, Byte, Word, DWord, QWord, Buffer), the lock rule in bit 4 (the Global Lock), the update rule in bits 5–6 (Preserve, WriteAsOnes, WriteAsZeros). Method flags (PDF 1136): argument count in bits 0–2, serialized in bit 3, sync level in bits 4–7. Mutex flags: sync level in bits 0–3.
- **§20.4** (PDF 1147–1148) works an example of how relative, parent-prefixed and absolute paths land in the namespace when a block is loaded under a node other than the root.

## 3. The namespace and loading (S1 §5.3, §5.4; PDF 362–367)

- One namespace holds every definition block. A name that collides while a block loads is fatal to that load (§5.3).
- **Search rules** apply only to a relative path of a single segment: it is looked up in the current scope, then each parent in turn up to the root. A path with parent prefixes or several segments is resolved exactly, without search (§5.3, PDF 363). Going above the root finds nothing.
- Creating an object by a path needs every segment but the last to exist already; otherwise it is an error.
- Every object but a method's locals has global scope. A method's own names, and any it creates elsewhere with `Scope` or a full path, are **dynamic**: created when executed and destroyed when the method returns (§5.5.2.3, PDF 370–371). A non-serialized method that blocks and is re-entered fails when it creates the same name again (§5.4.2, PDF 366).
- **Loading** runs the block's term list at "definition block level" in the root scope (§5.4.2, PDF 366–367). Any term, not only declarations, may appear there, so `If` around `Device` builds the namespace conditionally; a block is interpreted while it loads much as a method is when it runs.
- Unnamed objects cannot be created in the root (§5.4.2).
- Root scopes predefined by the OS (Table 5.220, PDF 363–364): `\_GPE`, `\_PR` (deprecated), `\_SB`, `\_SI`, `\_TZ`.

## 4. Control methods (S1 §5.5.2, PDF 369–371; §19.3.5.8, PDF 1011–1013)

- Up to seven arguments; their number is fixed by the method's declaration. A name that refers to a method, in an argument, is a call whose result is passed; a call that returns nothing there is fatal (§5.5.2.1).
- Arguments are passed by "reference constant" (§5.5.2.2): not copied, but not writable. A store to an `ArgX` replaces what the argument holds, as for a local, unless it holds a reference made by `RefOf`, in which case the store goes through to the referenced object. A called method changes its caller's buffers and packages only through an `Index` or field it creates on them.
- Eight locals, uninitialized at entry (§5.5.2.3, §19.6.79). Using an uninitialized object as an operand is fatal (Table 19.5).
- One object may be returned. Without a `Return` the value is undefined (§19.6.120, PDF 1105); a compiler emits `Return (Zero)` for a bare `Return`.
- Methods are not pre-empted but may block, and another method may run while one does (§5.5.2). A serialized method holds an implicit mutex at its declared sync level (§19.6.85, PDF 1076).
- Tables 19.9 to 19.14: reading an `ArgX` that holds a reference dereferences it; reading a local or a named object that holds one gives the reference itself. A store to a local or argument replaces it without conversion; a store to a named object converts to the object's existing type; `CopyObject` replaces object and type alike. A field keeps its type: `CopyObject` to one takes only an Integer or a Buffer. How `origin-aml` reads "replaces" (the review of B1, F13; the text does not say what holders of the old object see): `CopyObject` to a name gives the name a new object, and a buffer field over the old one, an `Index` reference into it or a caller's argument keeps the old one; a `Store` to a named integer, string, buffer or package changes the object itself, so its holders see the change, as Table 19.7's conversions to "the object's existing type" read.

## 5. Data types and conversions (S1 §19.3.5, PDF 1004–1013)

- Types (Table 19.5): Uninitialized, Buffer, Buffer Field, Debug object, Device, Event, Field Unit, Integer, Integer constant, Method, Mutex, Object reference, Operation region, Package (up to 255 elements in the table's text; `VarPackage` lifts that, §19.6.102), Power resource, Processor, raw data buffer (a compiler construct), String, Thermal zone.
- **Source conversion** (§19.3.5.4): an operand of the wrong type is converted to the one the operator wants; for `Concatenate` and the comparisons, the first operand's type decides the second's. If conversion is impossible the method is aborted.
- **Result conversion** (§19.3.5.5): explicit conversion operators and `CopyObject` store as is; a local or argument target takes the result as is; a named object or field converts the result to its own type.
- **Rules** (Table 19.7, PDF 1008–1010): Buffer to Integer copies up to the integer's width from the low byte up, zero-extending a short buffer; a zero-length buffer is refused. Integer to Buffer is 4 or 8 bytes. Integer to String is 8 or 16 hexadecimal digits. String to Integer reads hexadecimal digits from the first, stopping at the first other character or at the width; no `0x` prefix; an empty string is refused. String to Buffer copies the characters and the NUL, a zero-length string giving a zero-length buffer. Buffer to String is two hexadecimal digits per byte separated by spaces. Buffer fields and field units act as integers when they fit an integer, otherwise as buffers. A store into a buffer field or field unit truncates or zero-extends.
- Table 19.6 (PDF 1008): devices, events, methods, mutexes, regions, power resources, processors and thermal zones convert to nothing; storing to an integer constant is a no-op. (Compare §19.6.132, which calls a store to a read-only object fatal: see E13.)

## 6. Operators that need care (S1 §19.6)

Paraphrased; what each does that an implementation can get wrong.

- `Acquire` (PDF 1023): returns True on **timeout**; a timeout of 0xFFFF or more waits for ever; the same invocation may acquire a mutex it holds. `Wait` (PDF 1121) likewise returns non-zero on timeout. `Release` (PDF 1104) of a mutex not owned is fatal, and every mutex must be released before an invocation ends (§19.6.89, PDF 1078). Sync levels: an acquire must be at or above the current level, a release at it (§19.6.89).
- `Add`, `Subtract`, `Increment`, `Decrement` wrap (PDF 1023, 1034, 1060, 1111). `Multiply`'s overflow is "undefined" (PDF 1077). `Divide` and `Mod` by zero are fatal (PDF 1037, 1077). `ShiftLeft`/`ShiftRight` shift in zeros (PDF 1106–1107); counts at or past the width are not addressed. `FindSetLeftBit`/`FindSetRightBit` are one-based, 0 for no bit (PDF 1052–1053).
- `Buffer` (PDF 1026): the size is the larger of the declared size and the initializer's length; missing bytes are zero.
- `Package` (PDF 1083–1084): elements beyond the initializer are uninitialized. Named references to data objects are resolved at run time; names of non-data objects (device, method, mutex, region …) stay references.
- `Concatenate` (PDF 1027–1028, Tables 19.30 and 19.31): an integer first operand gives a buffer of both integers; a string gives a string; a buffer a buffer; any other type is first turned into a string naming its type ("[Device]" and so on, listed in Table 19.31).
- `ConcatenateResTemplate` (PDF 1029): appends the descriptors, then a new end tag and checksum; a one-byte source is a run-time error; an empty buffer counts as an end tag alone.
- `CondRefOf` (PDF 1029): True and a reference if the object exists, False and the target untouched if not. `RefOf` of a missing object is fatal (PDF 1102). `DerefOf` (PDF 1035) takes a reference or a string naming an object; dereferencing an uninitialized package element is fatal.
- `CopyObject` (PDF 1030–1031): see §4.
- `CreateField` and the fixed-size forms (PDF 1031–1032): the field must lie inside the buffer; zero bits is fatal.
- `DataTableRegion` (PDF 1033): a SystemMemory region over the table the XSDT lists with the given signature, OEM ID and OEM table ID, its length the table's.
- `Debug` (PDF 1034): write-only.
- `Fatal` (PDF 1050): the OS logs it and shuts down in an orderly way.
- `Field` (PDF 1050–1052, Table 19.34): fields are read normalized (shifted and masked); a field no wider than an integer is an Integer, wider a Buffer. Access widths per space are in Table 19.34: SMBus, IPMI and GenericSerialBus are buffer-protocol spaces; EmbeddedControl, SystemCMOS and GeneralPurposeIO byte-only. A `Lock` field takes the Global Lock, after the region's own mutex.
- `Index` (PDF 1061–1062): a reference to a package element, or a one-byte buffer field over a buffer's or a string's byte; a store through it changes that byte only.
- `IndexField` and `BankField` (PDF 1024–1025, 1063–1064): the index register is written with a byte offset aligned to the access width; the bank register with the bank value before each access.
- `LEqual`, `LGreater`, `LLess` and the `LNot` forms (PDF 1068–1070): integers numerically; strings and buffers byte by byte, unsigned, the shorter being less when one is a prefix of the other.
- `Load` (PDF 1070): from a SystemMemory region, a field or a buffer holding a table; the checksum is verified; loads at the root; returns a Boolean (zero on failure), also stored in its target. `LoadTable` (PDF 1071): finds a table in the XSDT by signature (≤ 4), OEM ID (≤ 6) and OEM table ID (≤ 8), loads it at a root path, then optionally stores a parameter. Tables meant for `LoadTable` must not be signed "SSDT", because the OS loads every SSDT itself.
- `Match` (PDF 1072): two comparisons against each element from a start index; elements that are not integers, strings or buffers, or that cannot be converted, never match; returns the index or `Ones`.
- `Method` (PDF 1075–1076): see §4.
- `Mid` (PDF 1077): an index at or past the end gives an empty result; a length past the end is cut.
- `Notify` (PDF 1080; Table 5.226, PDF 406–408): to a device, processor or thermal zone; 0x00–0x7F common to all types, 0x80–0xBF device-specific, 0xC0 and above hardware-vendor. Undefined values are ignored.
- `ObjectType` (PDF 1080–1081, Table 19.36): 0 to 16; a reference gives its target's type; typeless predefined scopes give 0.
- `OperationRegion` (PDF 1082): the region is for AML's exclusive use. A region declared inside a method is dynamic, and SystemIO or SystemMemory there is discouraged because the OS cannot reserve it.
- `Scope` (PDF 1105–1106): the target must exist and be the root, a predefined scope, a device, processor, thermal zone or power resource.
- `SizeOf` (PDF 1107): bytes of a buffer, characters of a string without the NUL, elements of a package; through a reference; anything else fatal.
- `Sleep` (PDF 1108) yields for at least the time; `Stall` (PDF 1109) must not yield, and delays over 100 µs should use `Sleep`. The grammar makes `Stall`'s operand a byte (PDF 1138).
- `Store` (PDF 1110): converts to the target's type; a store to Zero, One or Ones is not allowed; the result is what was written (a BufferAcc field may hand back modified data).
- `Switch` has no opcode: compilers emit `While (One)` with `If`/`Else` and `Break` (PDF 1112).
- `Timer` (PDF 1113–1114): monotonic, in 100 ns units.
- `ToBuffer`, `ToDecimalString`, `ToHexString`, `ToInteger`, `ToString` (PDF 1114–1118): `ToInteger` accepts decimal or `0x` hexadecimal strings (unlike implicit conversion); an empty string or buffer is illegal; a value too large is "unpredictable". `ToString` copies until the length or a NUL; `Ones` or no length means until a NUL.
- `While` (PDF 1122): creating the same name on a second iteration is fatal.

## 7. Predefined objects, device initialization and the Global Lock

- **§5.7** (PDF 424–429): `\_GL` is a mutex whose acquisition also takes the firmware-shared Global Lock; `\_OSI` is a method of one string argument answering Ones or Zero; `\_OS` a string naming the OS; `\_REV` an integer: 1 means ACPI 1 and 32-bit integers only, 2 means ACPI 2 or later, both widths, the DSDT's revision deciding which is used; other values reserved. Table 5.247 lists OS vendor prefixes ("FreeBSD", "HP-UX", "Linux", "OpenVMS", "Windows"); Table 5.248 the ACPI feature-group strings ("Module Device", "Processor Device", "3.0 Thermal Model", "Extended Address Space Descriptor", "3.0 _SCP Extensions", "Processor Aggregator Device"). `_OSI` is meant to ask about interfaces, not to identify the OS, yet the text itself notes that implementations other than Windows often answer True to every Windows string. `\_DLM` (PDF 428–429) names mutexes the OS shares with AML.
- **§5.8.1 `\_PIC`** (PDF 430): optional; 1 means APIC mode.
- **§6.3.7 `_STA`** (PDF 497): bit 0 present, 1 enabled, 2 shown, 3 functioning, 4 battery present; no `_STA` means all set. Its evaluation must not change platform state and may use only regions `_REG` has declared available.
- **§6.5.1 `_INI`** (PDF 547–548, Table 6.67): before a device's `_INI`, its `_STA`; present → run `_INI` and visit children; not present but functioning → skip `_INI`, visit children; neither → skip both. `\_SB._INI` runs unconditionally at the start of namespace initialization. A device that appears after its table was loaded does not get `_INI`.
- **§6.5.4 `_REG`** (PDF 549–551): run with the space ID and 1 or 0 when a space's handler connects or disconnects; methods must treat other regions as unavailable until then, writes being ignored and reads indeterminate. Always available, with no `_REG` run: PCI_Config on a root bus with `_BBN`, SystemIO, and SystemMemory within the memory map; and the EC of the ECDT before any method runs.
- **§5.2.10.1** (PDF 200–201): the Global Lock is a DWORD in the FACS with pending (bit 0) and owned (bit 1) bits, taken and released by the compare-and-exchange sequences shown there, the release signalling the other side when pending was set.
- **§5.6.4.1** (PDF 401–402): GPE handlers are `\_GPE._Exx` (edge) and `\_GPE._Lxx` (level); `_Qxx` for embedded-controller queries.

## 8. QEMU's own AML (Q)

Dumped on 2026-11-14 for five machines, each with `boot-test`'s options:

| Machine | `-smp` and extra | DSDT bytes | SHA-256 (first 16 hex) |
|---|---|---|---|
| default (Intel VT-d) | 2 | 8,504 | `394dd09b72a96199` |
| eight processors | 8 | 9,057 | `676737fb46f7ce98` |
| AMD-Vi | 2, `amd-iommu` | 8,521 | `849d9ec63a20baab` |
| sparse x2APIC | `1,maxcpus=150,…` and two cold-plugged CPUs | 23,923 | `9b866417d254a456` |
| legacy-free | 2, `pit=off,i8042=off` | 8,412 | `028fa7fa1a90105f` |

What the tables show:

- **No SSDT** on any of the five: the DSDT is the only definition block. The RSDP OVMF leaves at the 16-byte boundary is revision 0, with an RSDT; the FADT is revision 3.
- **The DSDT's revision is 1**, so the interpreter runs QEMU's AML with **32-bit integers** (§19.6.29).
- **`Processor` (0x5B 0x83) is used**: two on the default machine, 75 on the sparse x2APIC one, under `\_SB.CPUS`. ACPI 6.6 marks 0x5B 0x83 permanently reserved (E7).
- **The global lock is used**: `\_SB.HPET` has a SystemMemory region whose field has the `Lock` rule, and `\_SB.HPET._STA` reads it.
- **What else it uses**, counted by an opcode census written for this session from chapter 20 (identical set on all five machines): Name, Scope, Device, Processor, Method (11 serialized of 74 on the default machine; 0 to 5 arguments), Buffer, Package, the integer prefixes and String, Zero/One/Ones, OperationRegion (SystemMemory, SystemIO, PCI_Config), Field (byte and dword access; Preserve and WriteAsZeros; one with Lock), Mutex, Acquire, Release, Store, Subtract, Increment, And, Or, ShiftLeft, ShiftRight, If, Else, While, Break, Return, LEqual, LNotEqual, LLess, LGreater, LNot, LAnd, LOr, Index, DerefOf, SizeOf, ToBuffer, ToHexString, CreateDWordField, Notify, the Debug object, and method calls.
- **What it does not use**: `_INI`, `_OSI`, `External`, `Load`/`LoadTable`, `IndexField`, `BankField`, `Event`/`Wait`/`Signal`, `Sleep`/`Stall`, `Concatenate`, `Match`, `Mid`, `ToInteger`, `ToString`, `CopyObject`, `RefOf`/`CondRefOf`, `CreateField`, `PowerResource`, `ThermalZone`, `DataTableRegion`, `Fatal`, `Timer`, `Alias`, buffer-protocol regions. Every one of these is tested only with encoder-made blocks.
- **The first object is a `Scope`**, not a method (E9): `\DBUG` writes its argument as hexadecimal text, byte by byte, to a SystemIO region at port 0x402.
- Named results a read-only boot proof can check without any region access: `\_S5_` is a package of four zeros; `\PICF` starts at zero and `\_PIC (1)` sets it; `\_SB.PCI0._HID` is an EISA ID integer; `\_SB.PCI0._CRS` and `\_SB.HPET._CRS` are names holding buffers. Evaluations that **must** hit a region and be refused: `\_SB.HPET._STA` (SystemMemory, after the Global Lock), each processor's `_STA` (through `\_SB.CPUS.CSTA`, SystemIO), each `\_SB.LNKx._STA` (PCI_Config).
- **The largest DSDT the boots meet** (seen by the kernel's boot proof, step B3, 2026-11-14): that of the discovery machine, whose MADT lists 4,096 processors, 496,594 bytes with 4,096 processor devices. The interpreter needs 11,897,534 bytes for it, in 32,516 nodes; it loads in 231 ms and the walk takes 118 ms under QEMU 11.1.3's software processor. It stopped loading at the boot proof's first memory limit of 4 MiB (`Limit(Memory)` at offset 0x427ab), and the coordinating session's ruling of 2026-11-14 raised that limit to 32 MiB (`config.rs`): a real platform's DSDT can be as large, and the proof should judge it, not refuse it.

**What the tests can and cannot show** (the review of B1, F15). The encoder (`src/tests/encode.rs`) shares no code with the parser, but this session wrote both from one reading of chapter 20, so a misreading common to both passes every encoder-made test. QEMU's five DSDTs are the only tables the parser has met that it did not make, and they use only the opcodes listed above; the test that loads them is run by hand, not by `check`. ADR-0019 A3's "fuzzed" is met by the randomized test of the contract's §7 (blocks from the grammar, mutated blocks and adversarial blocks, with progress floors and planted-fault controls); there is no coverage-guided fuzzing. The outside checks still to come are TASKS item 64's reading (the gaps in §9) and step B3's boot proof, which is to dump the tables it reads and load them in that test.

## 9. Open questions and named gaps

**Questions, and the coordinating session's rulings of 2026-11-14**

- **Q1. The errata.** Not read: uefi.org answered 403 with a bot check (S2). The coordinating session has asked the owner to save today's errata page or confirm there is none. Until then this ledger records the errata as **not read**.
- **Q2. `Processor`.** Ruled: accepted as an object of type 12, with the layout ACPI 6.6 still leaves in its grammar (`ProcID`, `PblkAddr` and `PblkLen`, PDF 1137) and QEMU's bytes show: a package length, a name, a processor ID byte, a P_BLK address dword and a length byte, then a term list. **This is a deliberate departure from 6.6's reserved code** (Table 20.2, PDF 1145), because deployed firmware and QEMU 11.1.3 use it and ADR-0019 A1 asks for what a platform can present.
- **Q3. QEMU's tables as test fixtures.** Ruled: not committed. The host tests use encoder-made blocks; the boot proof (step B3) reads QEMU's tables at boot; an optional host test reads dumps from the directory `ORIGIN_AML_TABLES` names.
- **Q4. `_OSI`, `_OS` and `_REV`** (ADR-0019 T5). **Decided: ADR-0019 Amendment 1, decision item 309**, accepted by the owner on 2026-11-14 ("Accept 309"): `Ones` for the 22 Windows strings of M1, `Zero` for everything else, `Linux` included; feature-group strings added only when built; `_REV` = 2; one table, a change to which is the owner's. **`_OS` = "Origin OS": decided by ADR-0019 Amendment 2, decision item 315** (the owner, 2026-11-19, "Use Origin's own name"), after TASKS 64 answered G10 and TASKS 72 ran a server's firmware under both answers and found no difference: its only readers of `_OS` reach it when `_OSI` is missing.
- **The gaps G1 to G12, and G13 to G19 added since** (G13 to G16 by the reviews of B1 and B2, G17 and G18 by step B4, G19 by its review), go to a separate session (TASKS 64), which reads ACPICA under CLAUDE.md §3 and writes only `docs/research/2026-11-14-aml-gaps-acpica.md`. This session reads that ledger, never ACPICA. **It answered on 2026-11-19** (ACPICA 20260930, at `659a8b8c` on `land/batch-6`), and the coordinating session ruled the same day (`t63-g-rulings.md`): follow ACPICA's default where it says deployed firmware or other implementations rely on it, never its lenient mode, and report every tolerance to the host. Step B5 builds those rulings, each change with a test; the choices are below, each citing the ledger. **Before step B5's code leaves its batch for `main`, CLAUDE.md §6's similarity check runs against the files TASKS 64's ledger lists in its §5.** Steps B1 to B4 were written before any ACPICA reading and are not in it (decision item 121; the coordinating session's ruling of 2026-11-19), so the sixth batch, which holds B3, B4 and the ledger but no code derived from it, goes to `main` without it.

**Gaps the specification leaves, which a separate session may answer from ACPICA under CLAUDE.md §3 and ADR-0019 A4**

- **G1.** What an interpreter does with a name in an argument position that resolves to nothing at load time (a forward reference to a method defined later in the same block, or in a later SSDT without `External`): how many terms it consumes, and whether the load fails.
- **G2.** Whether deployed interpreters enforce mutex sync-level ordering strictly or only warn, and what they do with a method that returns holding a mutex.
- **G3.** What a load does with a duplicate name in a later SSDT (§5.3 calls it fatal; deployed firmware is said to have them): abort the whole table, or skip the object.
- **G4.** The exact text of `ToHexString`, `ToDecimalString` and the implicit Integer/Buffer-to-String conversions: letter case, `0x` prefixes, separators. §19.6.140 says commas for `ToHexString`, Table 19.7 spaces for an implicit Buffer-to-String.
- **G5.** The width used for `AnyAcc`, and how a field that straddles its access width or its region's end is split into accesses.
- **G6.** The value a caller sees when a method ends without `Return` and its result is used.
- **G7.** `Stall` with an operand above 255, and `Sleep`/`Stall` with very large values: refused, clamped or honoured.
- **G8.** Whether `Unload` (formerly 0x5B 0x2A, now reserved in Table 20.2) and the DDB handle still occur in deployed firmware, and what an interpreter should do on meeting them.
- **G9.** `ShiftLeft`/`ShiftRight` by a count at or beyond the integer width, and `Multiply` overflow (§19.6.88 says undefined).
- **G10.** What deployed firmware in scope expects `\_OS` to return.
- **G11.** A `Store` of a buffer into a named buffer of another length: whether the target keeps its length (as Table 19.7 says for integer and string sources) or takes the source's.
- **G12.** What `^` means in a method's body. §19.6.85 makes a method a scope of its own ("Method opens a name scope"), so `^X` from a method in `Device (D1)` would be `D1.X`; whether deployed firmware is written expecting that, or expecting `^` to leave the device.
- **G13.** Whether deployed firmware has string literals with bytes above 0x7F (§20.2.3 allows 0x01–0x7F), for example a vendor name with an accented letter or a trademark sign, and what deployed interpreters do with one. Refusing it fails the whole table (raised by the review of B1, F11).
- **G14.** Whether deployed firmware has a `Package` or `VarPackage` with more elements than its count (§19.6.102 makes it an ASL compile-time error, which says nothing of AML a compiler did not check), and what deployed interpreters do: refuse, keep the first elements, or grow the package. Found while fixing the review's F4: the randomized test's generator wrote such packages.
- **G15.** A string stored to a field unit: whether it is written a character at a time, each zero-extended to the field (the literal reading of Table 19.7, so `Store ("ABCD", F32)` leaves the field `0x44`), or its bytes laid into the field as a buffer's would be (raised by the review of B2, B2-13).
- **G16.** A field whose access width is wider than its region (a `DWordAcc` field of 8 bits in a 3-byte region): whether deployed interpreters narrow the access to fit, or refuse it (raised by the review of B2, B2-8).
- **G17.** After `Load` or `LoadTable` adds a table at run time, whether deployed operating systems run `_INI` for its devices and `_REG` for its operation regions, and when (B4's contract, Q9). §19.6.77 and §19.6.78 (PDF 1070, 1071) say only that the new block's control methods are not run while it loads. The library runs neither: that is for the ACPI service (ADR-0019 A2) to decide.
- **G18.** How `LoadTable`'s strings are compared with a table's fixed-length ID fields. §19.6.78 (PDF 1071) makes the comparisons case-sensitive and a string longer than its field an error, but says nothing of a shorter string: whether the table's field must then hold NUL, spaces or anything after it. Tables pad their IDs with spaces as a rule (QEMU's `"BXPC    "`).
- **G19.** What deployed systems do with names a run-time load placed under a parent that is later removed: a method's temporary object, or an object of a table whose own load later fails (raised by the review of B4, B4-3). §19.6.77 and §19.6.78 say nothing of it.

**The choices, after TASKS 64's ledger** (`docs/research/2026-11-14-aml-gaps-acpica.md` at `659a8b8c`, cited "ledger 64"; the coordinating session's rulings of 2026-11-19, `t63-g-rulings.md`). Until then each was "strict until the reading session answers" (the contract, `t63-contract.md` §4). "Kept" means ACPICA's default is the same; "B5" means step B5 changes it to ACPICA's default, with the step that builds it.

| Gap | Choice | Since |
|---|---|---|
| G1 | **In load-time code**, a name in an operand position that resolves to nothing is a plain name taking no arguments; the statement holding it fails alone and is abandoned and reported, and the load goes on (the load policy below). **In a running method** it ends the method, as before. A name that resolves to a method is a call taking the method's declared arguments; method bodies are decoded only when they run, so a forward call in a method takes its callee's arguments. **A name in a package made by load-time code** is bound once, after every boot table has loaded (or at the end of a run-time load); one that still names nothing becomes an empty element and is reported. A package made inside a method resolves its names when they are read, as before | B5-2 (ledger 64 §2, §3 G1) |
| G2 | A mutex taken with `Acquire` outlives the method that took it, until the outermost evaluation ends; whatever is still held then is released and reported, and the evaluation does not fail. A `Serialized` method's own mutex goes when that method ends. Sync-level order stays strict on `Acquire`, `Release` and `Serialized` entry, with ACPI 4.0's release rule. The library never serializes a method because it creates names, so no such method needs its level waived | B5-1 (ledger 64 §3 G2; TASKS 72's finding 1) |
| G3 | In load-time code a duplicate name refuses only its own definition, keeps the first object, skips a scope-opener's body, and is reported; the load and the other tables go on. In a running method it ends the method, as before. `Scope` over an Integer, String or Buffer stays refused (a statement failure) | B5-2 (ledger 64 §2, §3 G3) |
| G4 | Upper-case hex digits. Integer to string (implicit): the width's digits with leading zeros, no prefix. `ToHexString` of an integer: `0x` and no leading zeros (`0x0`). `ToDecimalString` of an integer: decimal (`0`). Buffer to string (implicit): `0x` and two digits a byte, separated by a space. `ToHexString` of a buffer: the same with commas and no spaces. `ToDecimalString` of a buffer: decimal bytes and commas. An empty buffer gives an empty string. String to integer never fails: white space (ASCII space, 0x09 to 0x0D) and leading zeros skipped; implicit always hex with `0x` tolerated, `ToInteger` decimal unless `0x`; stops at the first bad character or at the digit that would overflow, keeping the value so far; empty gives 0. String to buffer copies the terminating NUL, an empty string giving one zero byte | B5-4 (ledger 64 §3 G4) |
| G5 | `AnyAcc` (and `BufferAcc` in a byte-addressed space) is byte access. A field is accessed in naturally aligned units of its access width from the unit holding its first bit, the update rule applying to partly covered units. An Embedded Controller field wider than 8 bits is one host transfer of its bytes, up to 255 (`Range` above) | B5-5 (ledger 64 §3 G5; TASKS 72's finding 2) |
| G6 | A method that ends without `Return` gives no value; using it as an operand is an error | Kept (ledger 64 §3 G6: ACPICA's default; its implicit return is its lenient mode only) |
| G7 | `Stall` above 255 µs refused; 101 to 255 µs made and reported | Kept; the report is B5-3 (ledger 64 §3 G7) |
| G8 | 0x5B 0x2A (`Unload`) refused | Kept (ledger 64 §3 G8: ACPICA's own path ends in an error) |
| G9 | A shift by the width or more gives 0; `Multiply` keeps the low bits | Kept (ledger 64 §3 G9) |
| G11 | A named buffer keeps its length on a store, except that a zero-length one takes the source's; a named string takes the new string's length | B5-4 (ledger 64 §3 G11) |
| G12 | A method is a scope of its own: `^` from its body is the device or scope it is in | Kept (ledger 64 §3 G12) |
| G13 | A string literal takes any non-zero byte; a byte above 0x7F is reported. A literal with no NUL before its enclosing range's end ends there and is reported; the table's bytes are never written | B5-3 (ledger 64 §3 G13) |
| G14 | A package keeps exactly its count; elements past it are decoded, dropped and reported. An element that is an expression stays refused | B5-3 (ledger 64 §3 G14) |
| G15 | A string is written to a field as its bytes, without its NUL, zero-extended or cut to the field | B5-4 (ledger 64 §3 G15) |
| G16 | A unit of the field's access width that passes the region's end is refused (`Range`); no access is narrowed | Kept (ledger 64 §3 G16: the lenient mode alone narrows) |
| G17 | Neither `_INI` nor `_REG` runs for what `Load` or `LoadTable` adds; the library leaves both to the ACPI service, whose contract will carry ACPICA's behaviour as requirements: `_REG(connect)` for each new region whose space has a handler, a refresh of the GPEs, and a notice to the host | Kept (ledger 64 §3 G17) |
| G18 | `LoadTable`'s and `DataTableRegion`'s signature must be four AML name characters (`Name` otherwise), only four compared. An OEM ID or OEM table ID longer than its field is `Range`; an empty one matches any table; otherwise it is padded with zero bytes and all 6 or 8 bytes compared exactly, so a table's trailing spaces must be given. The host finds the first match in root-table order; no match gives Zero | B5-6 (ledger 64 §3 G18; replaces the B4 rule of the hand-over's point 7) |
| G19 | The names go with their parent, as every name does; the table that made them stays, not failed (the coordinating session's ruling on B4-3) | Kept (ledger 64 §3 G19) |

**The load policy** (ledger 64 §2; built in step B5-2). Load-time code is a table's table level and the bodies of the `Scope`, `Device`, `Processor`, `PowerResource`, `ThermalZone`, `If`, `Else` and `While` in it, not the methods it calls.
- A statement there that fails is abandoned whole: it is skipped to its structural end by parsing it again from its start, running nothing, creating nothing and asking the host nothing (names are looked up only to learn a method's argument count; an unresolved name takes none), a unit of work a term. The failure is reported, and the load goes on at the next statement. A failing scope-opener loses its whole body; a failing `If` or `While` predicate, on any iteration, skips the whole `If` or `While` and an `Else` right after the `If`. **Skipping the whole statement is Origin's choice**: ledger 64 says parsing goes on from where the failure stopped but did not read the unwinding in detail (its §5); for a name that is the whole statement, the common forward call, the two agree.
- What the parser cannot step past fails the table whole and rolls it back, as before: `Truncated`, `PackageLength`, `Reserved`, `Name`, `Grammar`, any limit, and `Fatal`. Every other kind is a statement failure; B4-13's refusal of locals and arguments has a kind of its own, `OutsideMethod`, so it abandons only its statement.
- A block byte-identical to a table already loaded and not failed is refused whole, before anything is created: `Load` and `LoadTable` give Zero, a boot load returns `Duplicate`, and it is reported (replaces B4's Q7).

**Reports** (built in step B5-1). Each tolerance is told to the host through `Host::report`: a statement abandoned, a table already loaded, a package name left unbound, extra package elements, an unterminated string, a string byte above 0x7F, a `Stall` of 101 to 255 µs, and a mutex released at an evaluation's end. At most 64 a top-level evaluation, then one `Suppressed` with the count; `TableInfo.abandoned` counts a table's abandoned statements in full.

**`Load` and `LoadTable`: what step B4 built, and its strict choices** (S1 §19.6.77 and §19.6.78, PDF 1070 and 1071, read again on 2026-11-18; B4's contract, accepted with its twelve questions as recommended the same day). Paraphrased:

- **What the text asks.**
  - `Load` takes an operation region in SystemMemory, a field of one, or a buffer, holding a definition block wholly inside it. The OS reads the table into memory, verifies its checksum and loads it into the namespace, by default at the root. Its Boolean result, zero for failure, goes to the optional target and is the operator's value too.
  - `LoadTable` searches the XSDT by signature, OEM ID and OEM table ID, case-sensitively. A string longer than 4, 6 or 8 characters is a run-time error. The root path is evaluated by the usual scoping rules from the current scope; when none is given the root is meant (the extracted text loses its backslash, as it does in the next paragraph's "backslash" character). After the table is added, `ParameterData` is stored into `ParameterPathString`: taken as written after a backslash or caret, else under the root path. If that object does not exist, a run-time error results.
  - For both: the table is in memory marked reserved or NVS, the load is synchronous, and none of the new block's control methods runs while it loads. The note that the OS loads the DSDT and every SSDT itself, so tables meant for `LoadTable` are signed otherwise, is §19.6.78's.
- **Strict choices** (each marked so in `load.rs`):
  - **Q2.** `Load` takes a block signed `SSDT` only (§19.6.77's "should"). Another signature gives Zero.
  - **Q3.** A failed check, or a term list that fails other than by a limit or a `Fatal`, rolls the table back whole and gives Zero. A limit ends the evaluation, and the table goes with it. **So does a `Fatal`** (the coordinating session's ruling on the review of B4, B4-2, clarifying Q3): §19.6.47 (PDF 1050) asks the OS to log the error and shut down, so no more firmware code runs in that evaluation, a loaded table's included.
  - **Q4.** True is Ones at the integer width.
  - **Q6.** `LoadTable` of a `DSDT` or `SSDT` key gives Zero without asking the host (§19.6.78's note).
  - **Q7.** A table loaded twice fails at its first duplicate name (as G3).
  - **Q8.** A load that completed stays when anything around it later fails, a table that a block's own load-time code loaded included: Q8 applies to loads as well as to evaluations (found by the soak, N-soak-3; the coordinating session's ruling on the review of B4, B4-3).
  - **A name cannot outlive its parent** (the same ruling). Names a completed table placed under an object that another creator later removes go with that object, and the table is not marked failed. **The contract's §6 said no partial table is left; corrected: a failed load leaves no name of its own; a completed table can lose names whose parent is removed by another creator** (G19).
  - **A root path names a scope object** (the review of B4, B4-4). `LoadTable`'s root path must name what `Scope` takes: the root, a predefined scope, a device, processor, thermal zone or power resource. Anything else is `WrongObject`.
  - **Every store a load makes is ready before anything changes** (the review of B4, B4-1). `Load`'s target, and `LoadTable`'s parameter, may need a PCI_Config region's function found first. `Load`'s target is made ready before the source is read. `LoadTable`'s parameter is stored by a step of its own, pushed once the load has ended: it finds the node, makes it ready and stores, so any failure there, a failed function walk or a limit included, comes after the table was added and leaves it loaded (§19.6.78; the review's addendum, B4-12). A store that still asks to be put back fails closed (`Grammar`).
  - **A loaded table's table level has no locals or arguments** (the review's addendum, B4-13). `Local0` to `Local7` and `Arg0` to `Arg6` named at the table level of a table that `Load` or `LoadTable` is loading would be the method's that ran the operator. They are refused (`Grammar`), wherever they are named, so that load fails with Zero. A method that table calls has locals and arguments of its own. The same block loaded by the host fails `Grammar` at them too, having no method at all.
  - G17, G18 and G19, above.
- **What the library leaves to its host.**
  - Whether a region is in reserved or NVS memory: a host decides every region access (ADR-0019 A2).
  - The text's optional check of a database for a newer revision of the same table: not done.
- **How a region is read** (Q5). The whole span is checked first: it must lie in the address space, and the memory limit must hold it. Then it is read through the host in the widest naturally aligned accesses, each a unit of work, only within the region as declared, and once: `Load`'s target is made ready before the source is read. (Before the review of B4, B4-1, a target that needed a PCI function put the operator back after the read, and the region was read twice.)
- **What a failed run-time load keeps** (the review of B4, B4-5, accepted). Its table's entry stays for the interpreter's life, 48 bytes with its AML released, so that a stale method of it still answers `TableFailed`. The entries are charged to the memory limit, and their vector doubles. The review's R7c: 10,000 failed loads held 16,384 entries, 786,432 bytes. Under the service's 64 MiB limit, firmware that fails a load on every evaluation brings its interpreter to `Limit(Memory)` cleanly. The arithmetic, corrected after the review's addendum (B4-15, which found the first figure of some 700,000 low by about half): `reserve` doubles the vector while doubling fits within the limit, then grows it once to all the limit allows (`mem.rs`). So the entries can fill what the limit leaves: 64 MiB is 67,108,864 bytes, and 67,108,864 / 48 = 1,398,101. Less what else the interpreter holds (its namespace, objects and the tables that loaded), that is near 1.4 million failed loads. The ACPI service's long-lived interpreter needs failed slots reclaimed (TASKS item 63, "Not covered").
- **Cost** (B4's contract): the check of a table before it loads is `origin-acpi-tables`', counted in CL41 as an indicative host figure.


**Inconsistencies in the ACPI 6.6 text found while reading** (for the errata check of Q1; each is resolved by a choice the contract names)

- **E1.** The `NamedObj` production (PDF 1134) omits `DefDevice`, `DefEvent`, `DefField`, `DefIndexField`, `DefMethod` and `DefMutex`, though each is defined below it.
- **E2.** `DefLoad` appears in no production (neither statement nor expression), though §19.6.77 gives it a Boolean result.
- **E3.** `CondRefOf`'s operands: `SuperName Target` in the grammar (PDF 1139), `SuperName SuperName` in Table 20.2 (PDF 1144).
- **E4.** `Load`'s second operand: `Target` in the grammar (PDF 1140), `SuperName` in Table 20.2.
- **E5.** Table 20.2 marks 0x5B 0x80–0x5B 0xFF reserved (PDF 1145), over the opcodes 0x5B 0x80–0x88 it defines just above.
- **E6.** `ReferenceTypeOpcode` names `UserTermObj` (PDF 1139), defined nowhere; a method invocation is the evident meaning.
- **E7.** 0x5B 0x83 (`Processor`) is permanently reserved (PDF 1145) and Table 19.36 makes object type 12 reserved, but Table 19.31 still names type 12 "Processor", §5.4.2 and §19.6.122 still speak of `Processor` scopes, and QEMU 11.1.3 emits the opcode.
- **E8.** `DataRegionOp` is written with "ExOpPrefix" (PDF 1136).
- **E9.** §5.4.2 says the first object in a definition block must be a named control method (PDF 366); QEMU's DSDT begins with a `Scope`.
- **E10.** `ConnectField`'s second alternative lacks its closing bracket (PDF 1135).
- **E11.** `Increment` overflows to zero, `Decrement` underflows to `Ones`, `Add`/`Subtract` lose the high bits, and `Multiply`'s overflow is undefined (PDF 1023, 1034, 1060, 1077).
- **E12.** `Mid` on a string speaks of "the length of the buffer" (PDF 1077).
- **E13.** A store to a constant is "a no-op, not an error" in Table 19.6 (PDF 1008) but "not allowed" in §19.6.132 (PDF 1110), which also calls a store to a read-only object fatal.
- **E14.** §5.7.2 refers to "Table 5-186" and "Table 5-187" for the strings now in Tables 5.247 and 5.248 (PDF 425).
- **E15.** `Stall`'s operand is `TermArg => ByteData` (PDF 1138), an integer that must fit a byte; §19.6.129 only advises against more than 100 µs.

## 10. What this ledger does not decide

The crate, the host interface, the object model and the limits are in the contract for TASKS item 63 (`t63-contract.md` in the coordination notes), accepted by the coordinating session on 2026-11-14 with the rulings above; TASKS item 63's entry in `docs/engineering/TASKS.md` records them.
