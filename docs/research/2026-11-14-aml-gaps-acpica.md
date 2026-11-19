# Research ledger: what ACPICA does in the AML interpreter's named gaps G1 to G19 (TASKS item 64)

| | |
|---|---|
| Date | Read on 2026-11-19 (UTC), 01:24 to 01:45; written the same day. The file name keeps the date TASKS item 64 was given (2026-11-14). |
| For | TASKS item 64: the gaps that TASKS item 63's ledger (`docs/research/2026-11-14-aml-interpreter.md`, §9, as step B4 left it on `land/batch-6`) hands to a separate session. TASKS item 63 reads this ledger and never ACPICA. |
| Requires | CLAUDE.md §3 (reference code read apart from the code, facts only, today's version), §6 (provenance), §16 (research first); ADR-0019 A4 (order of sources for the interpreter: ACPI 6.6, then Microsoft's documents, then ACPICA under §3's split); decision items 91 to 97 and 121 |
| Source | ACPICA release **20260930**: tag `20260930`, commit `e85aa3bebaae81ed1e6d163511ac7e1eafe732cf` (tag dated 2026-09-30 10:10 UTC; release published 2026-09-30 10:18 UTC). It was the newest release and the newest tag on the repository's releases and tags pages on 2026-11-19; the one before it is 20260408. The repository is now `open-acpica/acpica`; `acpica/acpica` redirects there. The release's own version constant agrees (`source/include/acpixf.h` 17–19). |
| Licence | Every file read carries Intel's copyright for 1999–2026 and the SPDX expression `BSD-3-Clause OR GPL-2.0-only` (for example `source/components/executer/exsystem.c` 9–10). **This ledger relies on the BSD-3-Clause option.** |
| Method | Each file was opened in the built-in browser at the commit above, through GitHub's raw view, and `documents/changes.txt` through GitHub's file page (its raw view would not open in the browser). Line numbers were counted in the page. Nothing was cloned, downloaded or saved on this machine; no Linux, QEMU or other project's source was opened. |
| Rules kept | Facts only, in Origin's words and in this ledger's own order: behaviours, conditions, values and sequences. No code, no identifiers or function names, and no arrangement of ACPICA's files or functions; each fact cites its file and lines. Where ACPICA's comments or change log say what deployed firmware relies on, that is paraphrased and cited, never quoted. The session that wrote this writes no Origin code, now or later. |

Every citation is a path in the ACPICA tree at commit `e85aa3b…` and a line range in that file. "The change log" is `documents/changes.txt`; its entries are cited with the release they belong to. Section 5 lists every file read.

## 1. The answers in one line each

"Strict choice" is TASKS item 63's choice marked "strict until the reading session answers" (its ledger, §9). Whether to change one is the coordinating session's ruling, not this ledger's.

| Gap | What ACPICA 20260930 does | Strict choice |
|---|---|---|
| G1 | While a table loads, a name that resolves to nothing abandons only the statement it is in; the load goes on. It is a plain name taking no arguments. Method bodies are parsed only when called, so forward calls inside methods work. Package elements are resolved after all tables load. | Differs at load time; same inside methods |
| G2 | Sync levels enforced strictly with no lenient mode. A method may return holding a mutex: no error, and it is released silently only when the outermost evaluation ends. | Same on order; differs on a mutex held at return |
| G3 | A duplicate name is skipped: the first object stays, and a duplicate Device or Method has its whole body skipped. The table and every other table still load. | Differs |
| G4 | Hex digits are upper case. Implicit buffer to string gives `0x` before each byte, separated by spaces. `ToHexString` of a buffer gives `0x` before each byte, separated by commas; of an integer, `0x` and no leading zeros. String to integer never fails. | Differs on `0x` and on overflow |
| G5 | `AnyAcc` means byte access. A field is split into access-width units, aligned down to the access width, with partial units merged by the update rule. | Differs |
| G6 | By default, using a missing return value is an error that ends the caller. In the optional lenient mode, the last value computed, or 0. | Same as ACPICA's default |
| G7 | `Stall` over 255 µs is an error and does not stall; 101–255 µs warns and stalls. `Sleep` is capped at 2 s. | Same |
| G8 | 0x5B 0x2A is still decoded as `Unload`. It logs a warning and an error, and since a handle no longer reaches AML, the call fails. It has never been seen in the field. | Same in effect |
| G9 | A shift by the integer width or more gives 0. `Multiply` keeps the low bits. | Same |
| G10 | `_OS` is "Microsoft Windows NT", which deployed firmware is said to expect. A host may override it. | `_OS` is still open |
| G11 | A named buffer keeps its length (zero-fill or truncate), except that a zero-length buffer takes the source's length. | Same, except zero length |
| G12 | A method is a scope, so `^` from its body names the Device it is in. | Same |
| G13 | String bytes are not checked: any non-zero byte is accepted. | Differs |
| G14 | Extra elements are dropped with an information message, never an error. Firmware is said to change `NumElements` on the fly. | Differs |
| G15 | A string's bytes go into the field as a buffer would, zero-extended or cut to the field. | Differs |
| G16 | Refused with an error; never narrowed. The lenient mode only allows the region's length rounded up. | Same |
| G17 | After a run-time load, `_INI` is not run, but `_REG(connect)` is, for each new region whose space has a handler. GPEs are refreshed and the host is told. | Same on `_INI`. ACPICA runs `_REG` itself, where Origin leaves it to the ACPI service |
| G18 | The given ID is padded with zero bytes and compared byte for byte, so spaces don't match. An empty ID matches any table. | Differs |
| G19 | A removed parent takes all its children, whoever made them; the table stays loaded. | Same |

## 2. Facts several gaps rest on

**How a table loads.**
- A definition block is loaded by running the whole table once, in order, as one method marked as module-level code. Module-level code runs in place as it is met. ACPICA says other implementations do the same and that some AML depends on it (`source/components/namespace/nsparse.c` 27–48, 263–301; `source/components/parser/psxface.c` 251–343). The change log dates this design to release 20180209 and gives the same reasons: ordering problems in ASL, and compatibility (`documents/changes.txt` 2273–2280).
- A method's body is not parsed when its table loads. Its place and length are saved, and it is parsed when it is first run (`source/components/parser/psloop.c` 144–163). ACPICA's reason: a call to a method defined further on cannot be parsed until the callee's argument count is known, which is only certain once the namespace is loaded (`source/components/namespace/nsload.c` 79–88).
- **Errors while a table loads are handled on a best-effort basis.** Any of these abandons the statement it occurred in, logs a message that the error is ignored and the load continues, and parsing goes on from where it stopped (`source/components/parser/psloop.c` 304–381, 407–459, 533–561; `source/components/parser/psobject.c` 580–639):
  - a name that already exists;
  - a name that is not found;
  - any other exception from executing a statement.

  Two special cases:
  - If the failing statement opens a scope (`Scope`, `Device`, `Method`, `PowerResource`, `Processor`, `ThermalZone`), its whole body is skipped, because the object may not exist (`source/components/parser/psloop.c` 348–378).
  - If the failure is in the predicate of `If` or `While`, that body is skipped, and so is an `Else` right after it (`source/components/parser/psloop.c` 421–456).

  The change log says this is deliberate: one run-time error should not lose the rest of a table, and the errors most often seen in the field are direct references to undefined names in module-level code (`documents/changes.txt` 2075–2088, release 20180531; 1969–1977, release 20180810). A method behaves differently: there, any error aborts the whole method (`source/components/parser/psparse.c` 554–570).
- At boot, the DSDT loads first, then every SSDT, PSDT and OSDT in the order of the root table list, each at the root. A table that fails is counted and the others still load. The result is a summary message with the counts of loaded and failed tables (`source/components/tables/tbxfload.c` 133–244).
- Only if a table's run fails as a whole are all the names it created removed (`source/components/namespace/nsload.c` 93–109). A table already loaded cannot be loaded again (`source/components/namespace/nsload.c` 62–68).

**The lenient ("slack") mode.** It is off by default; with it off, ACPICA aims to follow the specification strictly (`source/include/acpixf.h` 96–110). Switching it on allows five things, and adds warnings for behaviour the specification does not allow:
1. the implicit return of a method's last value;
2. field access past the end of a region;
3. reading an uninitialised local or argument as integer 0;
4. any object type as `Store`'s source;
5. unresolved names inside packages.

Two other defaults matter here: methods that create names are serialized automatically, which is on (`source/include/acpixf.h` 112–119), and `_OSI` exists, which is on (`source/include/acpixf.h` 121–126).

**A host can override an error in a method.** If the host installs an exception handler, it may turn a method's error into success, and the method then goes on (`source/components/dispatcher/dsmethod.c` 178–255).

## 3. The gaps

### G1. A name that resolves to nothing, in an operand position

**Question.** A name in an argument position that resolves to nothing at load time, such as a forward reference to a method defined later or in a later SSDT. How many terms does it consume, and does the load fail?

**What ACPICA does.**
- **How a name in an operand position is looked up.** It is looked up when it is parsed, from the current scope upward, and never created (`source/components/parser/psargs.c` 293–303). If it names a method and a call is possible there, it becomes a call taking the method's declared number of arguments (`source/components/parser/psargs.c` 305–366). Otherwise it is a plain reference to a name and takes **no** following terms.
- **When it resolves to nothing,** whether that is accepted depends on where it is:
  - In an old-style load pass that does not execute, it is allowed, and the comment says so to permit forward references (`source/components/parser/psargs.c` 372–380).
  - Under `CondRefOf`, it is allowed (`source/components/parser/psargs.c` 382–387).
  - As an element of `Package` or `VarPackage`, it is allowed at this point (`source/components/parser/psargs.c` 389–399).
  - Anywhere else, it is an error, and when executing, a method error (`source/components/parser/psargs.c` 402–415).
- **Because tables now load in a single executing pass** (section 2), a forward reference in module-level code to a method defined later is not taken as a call. That statement fails: it is abandoned, the error is logged, and the load continues from where parsing stopped (`source/components/parser/psloop.c` 407–459; `source/components/parser/psobject.c` 626–638). The bytes meant as that call's arguments are then read as what follows. The change log names such unresolved references as the most common module-level error seen in the field (`documents/changes.txt` 2084–2088).
- **Inside a method**, forward calls are not a problem: the body is parsed only when the method runs, by which time every boot table is loaded (`source/components/namespace/nsload.c` 79–88; `source/components/parser/psloop.c` 144–163). A name that still resolves to nothing aborts the method (`source/components/parser/psargs.c` 408–414).
- **Package elements in module-level code** are resolved later, so that they may refer forward, for compatibility with other implementations:
  - Their resolution is deferred and they are resolved after all tables are loaded (`source/components/dispatcher/dspkginit.c` 147–161, 226–236).
  - The namespace-wide object pass that runs after a table loads completes them. Only packages are deferred this way (`source/components/namespace/nsinit.c` 48–107, 426–446).
  - An element that is still unresolved becomes an empty element, with an error message (`source/components/dispatcher/dspkginit.c` 437–490).
  - The change log adds that existing firmware depends on this (`documents/changes.txt` 2613–2628, release 20170728; 2267–2271, release 20180209).
- **In a method**, package elements are resolved at once (`source/components/dispatcher/dspkginit.c` 226–236).

**Options.**
- None for the load policy, which is always on.
- A global setting can silence the message for unresolved package elements, which ACPICA says firmware carries in packages it never uses (`source/components/dispatcher/dspkginit.c` 443–461).
- The lenient mode covers unresolved package references in its list (`source/include/acpixf.h` 107).

**For an interpreter.**
- Parse a method's body only when the method is first run, or once all boot tables are loaded. That is the only point at which a call to a later method can be parsed with the right number of arguments.
- In module-level code, treat a name that resolves to nothing as a plain name taking no arguments. Fail only the statement that holds it, report it, and keep loading the table.
- In a running method, the same name is an error that ends the method.
- Resolve named references in module-level packages after all boot tables are loaded. An element that still resolves to nothing becomes empty and is reported.

### G2. Mutex sync levels, and a method that returns holding a mutex

**Question.** Do deployed interpreters enforce mutex sync-level ordering strictly, or only warn? And what happens to a method that returns while holding a mutex?

**What ACPICA does.**
- **Acquire.** The thread's current sync level is compared with the mutex's. If it is higher, `Acquire` fails with a mutex-order exception and an error message; there is no warning-only path (`source/components/executer/exmutex.c` 244–256).
  - On a first acquisition, the old level is saved and the thread takes the mutex's level (`source/components/executer/exmutex.c` 267–276).
  - The owning thread may acquire the same mutex again; ACPICA counts the depth (`source/components/executer/exmutex.c` 162–172).
- **Release** fails with an exception in three cases:
  - the mutex is not held (`source/components/executer/exmutex.c` 397–405);
  - it is released by a thread that does not own it, the Global Lock excepted (`source/components/executer/exmutex.c` 417–430);
  - its level is not the thread's current level. This is the ACPI 4.0 rule; before ACPI 4.0, a mutex at or below the current level could be released (`source/components/executer/exmutex.c` 432–447; `documents/changes.txt` 9689–9697, release 20090521).
- **Serialized methods.** On entry, a method declared `Serialized` makes the same level check and fails with the same exception (`source/components/dispatcher/dsmethod.c` 349–391). A method that is automatically serialized has its level set to 0 and the check skipped.
  - Automatic serialization applies to a method that creates names, and is on by default (`source/components/dispatcher/dsmethod.c` 41–61, 140–170; `source/components/dispatcher/dsinit.c` 99–139).
  - ACPICA's reason: imposing a level could break firmware that really uses sync levels. Its change log notes that sync levels are rarely used, and that imposing one caused mutex-order exceptions on some machines (`documents/changes.txt` 5106–5113, release 20140325; 3901–3903, release 20151124).
- **A method that returns holding a mutex** it acquired with `Acquire` is not an error, and the mutex is not released when the method returns.
  - Every mutex the thread still holds is released silently, with only a debug trace, when the outermost evaluation ends (`source/components/executer/exmutex.c` 489–555; `source/components/parser/psparse.c` 716–718).
  - ACPICA's stated purpose: one method may acquire a mutex and another release it, within one top-level evaluation (`source/components/executer/exmutex.c` 497–501).
  - A `Serialized` method's own mutex is released when that method ends (`source/components/dispatcher/dsmethod.c` 858–876).
- Every exception above aborts the method, unless the host's exception handler maps it to success (section 2).

**Options.** There is no lenient mode for sync levels. Automatic serialization can be switched off (`source/include/acpixf.h` 112–119).

**For an interpreter.**
- Enforce the order strictly, on `Acquire`, on `Release` and on entry to a `Serialized` method.
- Do not impose a sync level on a method serialized only because it creates names.
- Let a mutex outlive the method that acquired it, until the outermost evaluation ends. Then release whatever is still held, silently or with a diagnostic only, and do not fail the evaluation.

### G3. A duplicate name in a later SSDT

**Question.** What does a load do with a name that already exists, for example in a later SSDT? ACPI 6.6 §5.3 calls it fatal, but deployed firmware is said to contain such names. Does the whole table fail, or only that object?

**What ACPICA does.**
- **Creating a name that exists is an already-exists error.** This holds both while a table loads and while a method runs; only names created outside module-level code are temporary (`source/components/dispatcher/dswload2.c` 268–296, 339–344; `source/components/namespace/nssearch.c` 341–346). The existing object is left as it was.
- **While a table loads, that error abandons only the statement.** The first definition stays (section 2; `source/components/parser/psloop.c` 317–327, 546–561).
  - If the duplicate opens a scope (`Device`, `Method`, `PowerResource`, `Processor` or `ThermalZone`), its whole body is skipped with an information message, and nothing inside it is created (`source/components/parser/psloop.c` 348–378).
  - The table is not rolled back, and the other tables still load (`source/components/tables/tbxfload.c` 207–227).
- **Removing a whole table's names happens only if its run fails as a whole.** ACPICA names a namespace collision and a missing `Scope` target as such cases (`source/components/namespace/nsload.c` 93–109). After the 2018 changes, errors inside a table's statements no longer cause it.
- **A name declared only with `External` may be created once** in the older loading pass (`source/components/dispatcher/dswload.c` 322–359).
- **`Scope` over a name of type Integer, String or Buffer is allowed with a warning.** The name becomes untyped, for existing code that declares a name and then opens a scope on it. `Scope` over any other non-scope type is an error (`source/components/dispatcher/dswload2.c` 183–243).
- **An identical SSDT listed twice** in the XSDT/RSDT is detected and ignored; the change log says such XSDTs have been met on real machines. This is from the change log only; the code was not read (`documents/changes.txt` 2638–2639, release 20170728).
- **A duplicate name while a method runs** aborts the method. If the method is not serialized, ACPICA takes this as a sign the method cannot be re-entered: it marks the method serialized once its last thread leaves, with an information message (`source/components/parser/psparse.c` 572–586; `source/components/dispatcher/dsmethod.c` 907–944).

**Options.** A namespace-override flag lets a table replace existing objects. It is set only for a walk that asks to override, in the older loading pass, not for firmware tables (`source/components/dispatcher/dswload.c` 292–314; `source/components/namespace/nssearch.c` 315–339).

**For an interpreter.**
- While loading a table, refuse only the duplicate definition and keep the existing object.
- If the duplicate opens a scope, skip its whole body.
- Report it, keep loading the rest of the table and the remaining tables, and do not roll the table back.

### G4. The text of `ToHexString`, `ToDecimalString` and implicit conversions to String

**Question.** What exact text do `ToHexString`, `ToDecimalString` and the implicit Integer- and Buffer-to-String conversions give: letter case, `0x` prefixes, separators?

**What ACPICA does.**
- **Hex digits** are upper case, 0–9 and A–F (`source/components/utilities/uthex.c` 21–26).
- **Integer to String** (`source/components/executer/exconvrt.c` 449–517, using 286–403):

  | Conversion | Text |
  |---|---|
  | Implicit | Two hex digits for each byte of the integer width (16 digits, or 8 for a revision-1 table), leading zeros kept, no prefix |
  | `ToHexString` | `0x`, then the hex digits with leading zeros dropped, so zero gives `0x0` |
  | `ToDecimalString` | Decimal, leading zeros dropped, zero gives `0` |

- **Buffer to String** (`source/components/executer/exconvrt.c` 519–638):

  | Conversion | Text |
  |---|---|
  | Implicit | `0x` and two hex digits for each byte, separated by one space |
  | `ToHexString` | `0x` and two hex digits for each byte, separated by commas with no spaces |
  | `ToDecimalString` | Each byte in decimal without leading zeros, separated by commas with no spaces |

  An empty buffer gives an empty string.
  - ACPICA's comments date the `0x` on each byte to November 2018, for the implicit and the `ToHexString` cases alike (`source/components/executer/exconvrt.c` 564, 578).
  - The change log entry for that release covers `ToHexString`: it says the `0x` is for compatibility with other ACPI implementations, and that the specification is vague here (`documents/changes.txt` 1779–1784, release 20181213).
- **String to Integer** never raises an error (`source/components/utilities/utstrtoul64.c` 174–364; `source/components/utilities/utstrsuppt.c` 168–225, 259–283):

  | Step | Implicit conversion | `ToInteger` |
  |---|---|---|
  | Leading white space | Skipped | Skipped |
  | Base | Always hex. A `0x` prefix is tolerated as an extension, so `"10"` is sixteen | Decimal, unless the string starts with `0x` |
  | Leading zeros | Skipped | Skipped |
  | First character that is not a digit | Ends the conversion, keeping the value so far | Same |
  | A digit that would overflow the integer width | Ends the conversion, keeping the value of the earlier digits | Same |
  | Empty string | Gives 0 | Gives 0 |

  ACPICA notes that the specification calls overflow in `ToInteger` unpredictable. It chose to match the implicit case, with no error. No octal is accepted at run time.
- **String to Buffer** copies the string's terminating NUL into the new buffer, because existing AML depends on it (`source/components/executer/exconvrt.c` 233–255).
- **Buffer to Integer** takes up to the integer width's bytes, little-endian; an empty buffer is an error (`source/components/executer/exconvrt.c` 123–152).

**Options.** None.

**For an interpreter.**
- Produce exactly the texts in the two tables above.
- Treat string-to-integer conversions as total functions: stop at the first bad character or at the digit that would overflow, and give 0 for an empty string, never an error.

### G5. `AnyAcc`, and fields that straddle their access width

**Question.** What width does `AnyAcc` use, and how is a field that straddles its access width, or its region's end, split into accesses?

**What ACPICA does.**
- **`AnyAcc` is byte access**, with byte alignment, in a released build (`source/components/executer/exprep.c` 230–242).
  - A search for the widest suitable width exists only in a development build. ACPICA's note: it would need the region's length, which is not yet known when the field is created (`source/components/executer/exprep.c` 34–194, 57–61).
  - The comment describing `AnyAcc` as the widest necessary width matches only that compiled-out path (`source/components/executer/exprep.c` 346–349).
- **The other access types** have these widths: Byte and Buffer 1 byte, Word 2, DWord 4, QWord 8. An unknown access type fails the field's creation (`source/components/executer/exprep.c` 244–278, 356–361). A buffer field is always byte-aligned (`source/components/executer/exprep.c` 280–288).
- **Where the field starts.** Its first unit begins at the field's first byte rounded down to the access width's alignment; the field's start bit is counted from there (`source/components/executer/exprep.c` 368–388).
- **How it is accessed.** The field is accessed as consecutive units of the access width, from that first unit (`source/components/executer/exfldio.c` 760–765, 925–932; units are capped at 8 bytes, 750–756 and 911–915):
  - the count is the field's bit length plus its start bit, rounded up to whole units;
  - on a write, the first unit is masked below the start bit and the last above the field's end;
  - a field that exactly fills one unit at bit 0 is read in a single access (`source/components/executer/exfldio.c` 728–746).
  - A partly covered unit is written according to the field's update rule (`source/components/executer/exfldio.c` 586–675). With Preserve, the unit is read first and the other bits kept. With WriteAsOnes, the other bits are set to 1; with WriteAsZeros, to 0. An unknown rule is an error.
- **Each unit is checked against the region** before it is accessed; G16 has the details.
- **Embedded Controller fields** wider than 8 bits are read and written in one access as wide as the whole field, up to 255 bytes (`source/components/executer/exprep.c` 503–518).

**Options.** None for `AnyAcc`.

**For an interpreter.**
- Use byte access for `AnyAcc`.
- Split every field into naturally aligned units of its access width, starting at the unit that holds the field's first bit.
- Apply the update rule to partly covered units, reading the unit first for Preserve.
- Access Embedded Controller fields wider than a byte as one transfer.

### G6. The value of a method that ends without `Return`

**Question.** What value does a caller see when a method ends without `Return` and its result is used?

**What ACPICA does.**
- **In the default mode, a missing value is an error.** A method that ends without `Return` gives no value, and nothing is passed to the caller (`source/components/dispatcher/dsmethod.c` 689–740; `source/components/parser/psparse.c` 619–663). If the caller uses the value as an operand, the operand is missing: ACPICA reports a missing operand and a "no return value" exception, and the caller's method aborts (`source/components/dispatcher/dsutils.c` 652–669; `source/components/dispatcher/dswstate.c` 76–95).
  - A top-level evaluation simply receives no value (`source/components/parser/psparse.c` 682–696).
- **In the lenient mode**, two things change:
  - Every evaluation begins with an implicit value of integer 0, which ACPICA says is for Windows compatibility (`source/components/parser/psxface.c` 196–210; `source/components/parser/psparse.c` 631–652).
  - The result of each operator and each method call is saved as the method's implicit value, replacing the previous one, and so is each `If`/`While` predicate (`source/components/dispatcher/dsutils.c` 76–140, 174–183; `source/components/dispatcher/dswexec.c` 158–160).

  So the caller sees the last value computed, or 0. ACPICA calls this optional, because ASL does not define it. According to the change log:
  - the integer 0 for methods with no code was added for Windows compatibility, since stub methods seen in the field made drivers fail when they expected a value (`documents/changes.txt` 10215–10220, release 20081031);
  - the implicit return was made more like Microsoft's interpreter (`documents/changes.txt` 10312–10316, release 20080926).
- **A method that returns a reference to its own local or argument** returns no value instead (`source/components/dispatcher/dsmethod.c` 779–818).

**Options.** The lenient mode decides this, and it is off by default (`source/include/acpixf.h` 96–110).

**For an interpreter.**
- ACPICA's default: using the missing value is an error that ends the caller.
- ACPICA's documented compatibility with Windows: the caller gets the last value the method computed, or integer 0 if there was none.
- Choosing between the two is a policy decision, not something the specification settles.

### G7. Large `Stall` and `Sleep` values

**Question.** `Stall` with an operand above 255, and `Sleep` or `Stall` with very large values: refused, clamped or honoured?

**What ACPICA does.**
- **`Stall`** (`source/components/executer/exsystem.c` 126–176):
  - more than 255 µs is an error: it is logged once, raises an operand-value exception (which aborts the method), and no stall is made;
  - from 101 to 255 µs, a warning that this violates the specification is logged once, and the stall is made;
  - ACPICA allows up to 255 µs for compatibility with other interpreters and existing BIOSes;
  - the operand is cut to its low 32 bits before the check (`source/components/executer/exoparg1.c` 166–169);
  - the change log ties the 100 µs warning to the specification's rule (`documents/changes.txt` 383–389, release 20220331).
- **`Sleep`** (`source/components/executer/exsystem.c` 179–216; `source/include/acconfig.h` 111–113; `source/components/executer/exoparg1.c` 161–164):
  - the time is a full 64-bit number of milliseconds, capped at 2000 ms (two seconds), with no error;
  - ACPICA gives two reasons for the cap, compatibility with other implementations and avoiding accidentally long sleeps;
  - the interpreter is released while the thread sleeps;
  - the two-second cap is old; a 20-second cap had been a mistake (`documents/changes.txt` 7652–7655, release 20110922);
  - ACPICA's compiler, not its interpreter, warns about sleeps over 10 ms (`documents/changes.txt` 358–363).
- **Related:** a `While` loop is cut off after 30 seconds by default (`source/include/acconfig.h` 119–121).

**Options.** None.

**For an interpreter.**
- `Stall`: refuse more than 255 µs as an error; accept 101–255 µs with a diagnostic.
- `Sleep`: clamp to 2 s without an error, and let other evaluations run while it sleeps.

### G8. `Unload` (0x5B 0x2A) and the DDB handle

**Question.** Do `Unload` (formerly 0x5B 0x2A, now reserved in ACPI 6.6 Table 20.2) and the DDB handle still occur in deployed firmware, and what should an interpreter do on meeting them?

**What ACPICA does.**
- **The opcode is still decoded and executed.** 0x5B 0x2A is still defined as `Unload` (`source/include/amlcode.h` 145), and is dispatched to the unload routine (`source/components/executer/exoparg1.c` 171–174).
- **Two messages first.** The routine logs a warning that an unload was requested; the comment hopes to learn which machines use the operator, which it calls extremely rare. It then logs an error that `Unload` is not supported (`source/components/executer/exconfig.c` 711–728). The reasons, given in the comment and in the change log:
  - a correct implementation may not be possible on some hosts;
  - other implementations do not fully support it;
  - it would need host driver support that does not exist;
  - it has never been seen in the field (`documents/changes.txt` 2090–2096, release 20180531; 5132–5136, release 20140325).
- **It then still tries to unload.** It checks its operand, which must be a valid table handle not already unloaded, else an operand-type error. If the check passes, it unloads the table and invalidates the handle (`source/components/executer/exconfig.c` 730–768).
- **No handle reaches AML any more.** Since ACPI 6.4 semantics came in, `Load` stores a pass/fail integer and `LoadTable` returns one; the handle `Load` creates internally is released before it returns (`source/components/executer/exconfig.c` 395–402, 676–683; `documents/changes.txt` 368–371, release 20220331). So an `Unload` given that integer fails the handle check, which is an error.
- **Unknown opcodes in general** are skipped, one byte or two after 0x5B, with an error message, and parsing continues (`source/components/parser/psobject.c` 77–131).

**Options.** None.

**For an interpreter.** Treat 0x5B 0x2A as an error when it is met: ACPICA's own behaviour ends in one, and the operator has not been seen in deployed firmware.

### G9. Shifts by the integer width or more, and `Multiply` overflow

**Question.** What do `ShiftLeft` and `ShiftRight` give for a count at or beyond the integer width, and what does `Multiply` give on overflow (ACPI 6.6 §19.6.88 says undefined)?

**What ACPICA does.**
- **Shifts.** `ShiftLeft` and `ShiftRight` by a count at or above the integer width give 0, because the C language leaves such shifts undefined (`source/components/executer/exmisc.c` 315–337). The width is 32 bits for a revision-1 table, otherwise 64.
- **`Multiply`, `Add` and `Subtract`** are computed on 64-bit values with no overflow check, so the result is the low 64 bits (`source/components/executer/exmisc.c` 287–313, 339–341).
- **Revision-1 tables.** Every integer result is cut to 32 bits when executing from a revision-1 table (`source/components/dispatcher/dswexec.c` 734–738).

**Options.** None.

**For an interpreter.** A shift by the integer width or more gives 0, and `Multiply` keeps the low bits of the product at the table's integer width.

### G10. What `_OS` returns

**Question.** What does deployed firmware in scope expect `\_OS` to return?

**What ACPICA does.**
- **The value.** The root object `_OS` is the string "Microsoft Windows NT" (`source/include/acconfig.h` 35–43; `source/components/namespace/nsaccess.c` 86–98). ACPICA's comment gives the reasons:
  - `_OS` is essentially obsolete, but a great deal of existing AML compares it against that string;
  - that string usually sends the AML down its most-tested path;
  - some AML does not call `_OSI` unless `_OS` matches it;
  - changing it is at the integrator's own risk.
- **History.** The change log records the default as chosen in 2004 for maximum compatibility (`documents/changes.txt` 15481–15485, release 20040514).
- **The host may override it.** The value is set at build time, and at start-up the host may supply a different initial value for any predefined root object. A failed override is logged and the default kept (`source/components/namespace/nsaccess.c` 138–155).
- **Related.** The root object `_REV` is 2 (`source/components/namespace/nsaccess.c` 94), as decision item 309 already chose. `_OSI` exists by default (`source/include/acpixf.h` 121–126).

**For an interpreter.**
- Answering `_OS` with "Microsoft Windows NT" is what ACPICA says deployed firmware expects.
- Another string risks the less-tested paths, and on some machines a skipped `_OSI`.
- The value is the owner's decision (TASKS item 64; decision item 309's note).

### G11. `Store` of a buffer into a named buffer of another length

**Question.** When a buffer is stored into a named buffer of another length, does the target keep its own length (as ACPI 6.6 Table 19.7 says for integer and string sources) or take the source's?

**What ACPICA does.**
- **A store converts the value to the target's type,** so a named Integer, String or Buffer keeps its type. `CopyObject`, and a store to an argument, replace the object instead (`source/components/executer/exstore.c` 380–391, 500–550).
- **A buffer stored into an existing buffer** (`source/components/executer/exstorob.c` 36–125):

  | Case | Result |
  |---|---|
  | Source as long as the target or shorter | The target is cleared and the source copied in, so the rest is zero; the target keeps its length (78–84) |
  | Source longer | Only the target's length is copied (108–118) |
  | Target of length zero, or one whose data still lies in the table | The target takes new storage of the source's length (60–74) |

  - The overflow case is reported only at debug level.
  - Buffers created from AML have their own storage, so that last case arises for zero-length targets (`source/components/dispatcher/dsobject.c` 214–250).
  - A disabled block in that code records the history: ACPI before 3.0A required truncating the target when the source was a string, but other implementations never did, and that became the de facto standard (`source/components/executer/exstorob.c` 85–106).
- **A string stored into a string** gives the target the source's length (`source/components/executer/exstorob.c` 141–210).
- **A `Buffer` created in AML** is as long as the larger of its declared size and its initializer (`source/components/dispatcher/dsobject.c` 214–223).

**Options.** None.

**For an interpreter.**
- A named buffer keeps its length on a store: a shorter source is zero-filled and a longer one cut.
- A zero-length named buffer takes the source's length.
- A named string takes the new string's length.

### G12. `^` in a method's body

**Question.** What does `^` mean in a method's body? ACPI 6.6 §19.6.85 makes a method a scope of its own, so `^X` in a method inside `Device (D1)` would be `D1.X`. Is deployed firmware written expecting that, or expecting `^` to leave the device?

**What ACPICA does.**
- **The method is the scope.** When a method runs, its own node is made the current scope (`source/components/dispatcher/dswstate.c` 676–690). A method opens a scope, as a Device, PowerResource, Processor, ThermalZone and Scope do (`source/components/utilities/utdecode.c` 23–60). So the lookup starts at the method itself and does not move up to an enclosing scope (`source/components/namespace/nsaccess.c` 342–376).
- **What each `^` does.** Each `^` moves one level up from that starting point, and after any `^` there is no upward search (`source/components/namespace/nsaccess.c` 433–474).
- **Climbing past the root** is a not-found error, reported as too many parent prefixes.

So `^X` in a method inside `Device (D1)` is `D1.X`. While a table loads, the scope is the nearest enclosing named object (`source/components/dispatcher/dswstate.c` 701–735).

**Options.** None.

**For an interpreter.** Treat a method as a scope of its own: from its body, `^` names the object that contains the method.

### G13. String bytes above 0x7F

**Question.** Does deployed firmware have string literals with bytes above 0x7F (§20.2.3 allows only 0x01–0x7F), and what do deployed interpreters do with one?

**What ACPICA does.**
- **No check of the bytes.** A string literal is everything up to the first zero byte, and no byte value is checked, so 0x80–0xFF are accepted as they are (`source/components/parser/psargs.c` 545–558).
- **A missing terminator.** If no zero byte comes before the end of the current AML range, ACPICA warns, writes a zero into the last byte of that range in memory, and goes on (`source/components/parser/psargs.c` 559–580).
- **Firmware evidence.** No comment or change-log entry read for this ledger says whether deployed firmware has such bytes.

**Options.** None.

**For an interpreter.** Accepting any non-zero byte in a string literal is what ACPICA does. Refusing one would fail tables that ACPICA loads.

### G14. A `Package` with more elements than its count

**Question.** Does deployed firmware have a `Package` or `VarPackage` with more elements than its count (§19.6.102 makes this an ASL compile-time error, which says nothing of AML no compiler checked)? Do deployed interpreters refuse it, keep the first elements, or grow the package?

**What ACPICA does.**
- **The count is the length.** A package always has exactly its declared count of elements (`source/components/dispatcher/dspkginit.c` 36–61, 163–169).
- **More elements than the count.** The extra elements are created, then discarded, with an information message giving both numbers. No exception is raised (`source/components/dispatcher/dspkginit.c` 259–298). The comment cites the specification's statement that this is an error. Its reasons for accepting it anyway:
  - compatibility with other implementations;
  - some firmware changes the count in the AML on the fly, which produces such packages.
- **Fewer elements than the count.** The package is padded with uninitialised elements, as the specification says; this is not an error (`source/components/dispatcher/dspkginit.c` 299–312).
- **Related.**
  - An element that is an expression, rather than a constant or a name, is refused with an error. ACPICA says no known interpreter supports this, and the change log adds that it has never been seen (`source/components/dispatcher/dspkginit.c` 171–197; `documents/changes.txt` 1786–1789, release 20181213).
  - A method's name used as an element becomes a reference to it, not a call (`source/components/dispatcher/dspkginit.c` 199–208).

**Options.** None.

**For an interpreter.** Keep the first elements up to the count, drop the rest, and report it without failing.

### G15. A string stored to a field unit

**Question.** When a string is stored to a field unit, is it written a character at a time, each zero-extended to the field (the literal reading of Table 19.7, so `Store ("ABCD", F32)` leaves `0x44`)? Or are its bytes laid into the field as a buffer's would be?

**What ACPICA does.**
- **Fields take the data as it is.** A store to any field writes the source's data into it, and the field keeps its type (`source/components/executer/exstore.c` 552–564). For a source's data:
  - an Integer gives all 8 bytes of its value;
  - a Buffer gives its bytes;
  - a String gives its characters as bytes, at the string's length without the terminator, not a character at a time;
  - any other type is an operand-type error (`source/components/executer/exfield.c` 417–441).
- **Fitting the bytes to the field.** The bytes are then written into the field (`source/components/executer/exfldio.c` 877–1008):
  - fewer bytes than the field needs are padded with zeros, since a field is written whole and never partly;
  - more bytes are cut to the field.

So `Store ("ABCD", F32)` writes 0x41, 0x42, 0x43, 0x44 into the field's bytes in order, which is the little-endian value 0x44434241.

**Options.** None.

**For an interpreter.** Write a string to a field as its bytes, the way a buffer of the string's length would be written: zero-extended or cut to the field.

### G16. A field whose access width is wider than its region

**Question.** A field whose access width is wider than its region, such as a `DWordAcc` field of 8 bits in a 3-byte region: do deployed interpreters narrow the access to fit, or refuse it?

**What ACPICA does.**
- **The check.** Before each unit is accessed, the unit's offset plus the access width must lie within the region's length (`source/components/executer/exfldio.c` 138–145).
- **If it does not,** the access is not made:
  - if the access width is wider than the whole region, ACPICA first logs that the access width is too large for the region (`source/components/executer/exfldio.c` 164–178);
  - in every case it logs that the unit lies past the region's end and raises a region-limit exception (`source/components/executer/exfldio.c` 180–193).

  The access is never narrowed.
- **Lenient mode only.** The access is allowed if the unit lies within the region's length rounded up to the access width (`source/components/executer/exfldio.c` 147–162). The change log calls such fields a common coding error (`documents/changes.txt` 15295–15301, release 20040715).
- **The compiler.** ACPICA's ASL compiler also flags fields past their region's end when the region's length is known when the ASL is compiled (`documents/changes.txt` 19255–19260).

**Options.** The lenient mode, off by default.

**For an interpreter.** Refuse an access unit that passes the region's end, and never narrow it.

### G17. `_INI` and `_REG` after `Load` or `LoadTable`

**Question.** After `Load` or `LoadTable` adds a table at run time, do deployed operating systems run `_INI` for its devices and `_REG` for its operation regions, and when?

**What ACPICA does.**
- **Steps 1–3: the load itself.** A run-time load (`source/components/tables/tbdata.c` 1055–1110):
  1. runs the table as at boot (section 2);
  2. refreshes the GPEs for any new `_Lxx`/`_Exx` methods, ignoring errors. Finding new wake GPEs through `_PRW` is left to the host;
  3. calls the host's table-event handler with a load event.
- **Step 4: initialising the new objects.** Inside step 1, once the table has run, a pass over the objects the new table owns does two things, and runs no device methods (`source/components/namespace/nsload.c` 117–133; `source/components/dispatcher/dsinit.c` 37–156):
  - it initialises each new operation region. Failures are logged and the pass goes on;
  - it scans each method that is not `Serialized`, to serialize it automatically if it creates names.
- **Step 5: deferred packages.** `Load` and `LoadTable` then run the namespace-wide pass that completes deferred package elements. It also runs no `_INI` and no `_REG` (`source/components/executer/exconfig.c` 372–376, 670–674; `source/components/namespace/nsinit.c` 48–107).
- **`_REG` runs during step 4,** when a region is initialised and a handler for its address space is installed on its scope or any ancestor (`source/components/events/evrgnini.c` 562–693):
  - the region is attached to that handler and `_REG(space, 1)` runs at once;
  - with no handler yet, nothing happens until one is installed.
- **Conditions on that `_REG` call** (`source/components/events/evregion.c` 573–712):
  - it runs only after boot initialisation of the namespace has finished, and only for a region with a handler;
  - `_REG` is looked for in the region's parent scope;
  - a connect is never repeated, nor a disconnect made without one;
  - its return value is ignored.
- **SystemMemory and SystemIO on this path.** The bulk `_REG` pass used at boot and when a handler is installed skips SystemMemory, SystemIO and DataTable regions. The reasons: they are always accessible, and a DataTable region is not a real address space (`source/components/events/evregion.c` 741–753; `documents/changes.txt` 1884–1904, release 20180927). **The per-region call that step 4 uses has no such check** in the lines read, so ACPICA runs `_REG` for any region of a dynamically loaded table whose space has a handler.
- **`_INI` runs only at boot** (`source/components/namespace/nsinit.c` 112–268, 552–738):
  - the root `_INI` runs first, then `\_SB._INI`, which must come before any `_REG`;
  - then every `_REG`, a step ACPICA keeps so as not to break existing machines, though the specification needs it only for PCI_Config regions on a root bus without `_BBN`;
  - then the `_INI` of each present device, using `_STA`. A device whose `_STA` is missing is assumed present. A device neither present nor functioning is skipped with its children. One not present but functioning has its children examined.

  Nothing on the run-time load path runs `_INI`.
- **The order inside a method** is noted as a possible non-conformance. While a method runs, a region's `_REG` is run automatically only if `_REG` is defined before the region. During a table load it runs either way. ACPICA keeps this to avoid regressions (`source/components/events/evrgnini.c` 582–594).

**Options.** None on this path.

**For an interpreter and its host.** ACPICA's behaviour after a run-time load:
- run `_REG(space, 1)` for each new region whose address space has a handler, as the table's objects are initialised;
- run no `_INI` for the new devices;
- tell the host a table was loaded, so it can, for example, evaluate the new devices' `_PRW` for wake GPEs.

### G18. How `LoadTable`'s strings are compared with a table's ID fields

**Question.** How are `LoadTable`'s strings compared with a table's fixed-length ID fields? §19.6.78 makes the comparison case-sensitive and a string longer than its field an error, but says nothing of a shorter string: must the field then hold NUL, spaces, or anything after it? Tables pad their IDs with spaces as a rule (QEMU's `"BXPC    "`).

**What ACPICA does.**
- **Lengths.** An OEM ID longer than 6 characters, or an OEM table ID longer than 8, is a string-limit exception (`source/components/executer/exconfig.c` 267–276; `source/components/tables/tbfind.c` 61–67).
- **The signature.** Its first four characters must be valid name characters, otherwise the call fails with a bad-signature exception. Its length is not otherwise checked, and only four characters are compared (`source/components/tables/tbfind.c` 54–59).
- **Shorter IDs are padded with zero bytes, not spaces** (`source/components/tables/tbfind.c` 69–91).
- **The comparison** (`source/components/tables/tbfind.c` 93–141):
  - tables are searched in the order of the root table list, and the first match wins;
  - the signature must match its four bytes exactly;
  - an **empty** OEM ID matches any table, and so does an empty OEM table ID;
  - otherwise all 6 (or 8) bytes are compared byte for byte with the zero-padded string. So a table whose ID is padded with spaces does not match a shorter string; the string must give the spaces too.
- **No match is not an error.** `LoadTable` returns 0 (`source/components/executer/exconfig.c` 288–306, 257–265).

**Options.** None.

**For an interpreter.**
- Pad the given IDs with zero bytes and compare all bytes exactly, so spaces in the table must be given in the string.
- Treat an empty OEM ID, or OEM table ID, as matching any table.
- Return 0 when no table matches.

### G19. Names under a parent that is later removed

**Question.** What do deployed systems do with names that a run-time load placed under a parent which is later removed: a method's temporary object, or an object of a table whose own load later fails?

**What ACPICA does.**
- **When a method ends,** the names it created are removed, unless it is module-level code or other threads are still running it (`source/components/dispatcher/dsmethod.c` 820–856). Two passes do this:
  - everything below the method's own node, of any owner (`source/components/namespace/nsalloc.c` 357–447);
  - if the method created names elsewhere, every name with the method's owner (`source/components/namespace/nsalloc.c` 452–570).
- **Removing an owner's names.** The second pass walks the whole namespace (`source/components/namespace/nsalloc.c` 452–570):
  - objects of that owner are detached, and a childless node of that owner is removed;
  - when the pass leaves a node of that owner, the node is removed together with **all** its direct children, whoever created them (`source/components/namespace/nsalloc.c` 299–352).
  - A child that still has children of its own is reported with an error, then removed anyway (`source/components/namespace/nsalloc.c` 333–337).
- **What it means for a loaded table.** Its names placed under a removed object go with that object. Nothing in these passes changes the loaded table's state.
- **A table whose run fails as a whole** has all its names removed by the same owner pass, with the same effect on names other tables placed under them (`source/components/namespace/nsload.c` 93–109).

**Options.** None.

**For an interpreter.** A name never outlives its parent: removing an object removes everything below it, whoever created it. The table that created those names stays loaded.

## 4. Other facts met in the same files

Each of these bears on a choice TASKS item 63 has already made. They were met while reading for the gaps above, not looked for.

- **`LoadTable` looks up its parameter path before it loads** (`source/components/executer/exconfig.c` 330–353): relative to the root path, or the root when there is none, unless it begins with `\` or `^`.
  - A failed lookup is an exception, and the table is not loaded.
  - If the later store into the parameter fails, ACPICA unloads the table again and returns the error (`source/components/executer/exconfig.c` 378–393).
  - Step B4's answer to B4-12 instead leaves the table loaded.
  - On success, both `Load` and `LoadTable` give 64 one-bits (`source/components/executer/exconfig.c` 399–402, 680–683).
- **`Load`'s inputs** (`source/components/executer/exconfig.c` 491–517, 592–632):
  - its target must already be an Integer object, otherwise an operand-type error;
  - its source is a SystemMemory region or a buffer;
  - a buffer must hold at least a table header and the table's declared length;
  - the table is copied before it is installed.
- **Where `Load` puts the table.** At the root of the namespace. ACPICA says this seems to go against the specification but matches other implementations (`source/components/executer/exconfig.c` 657–663).
- **The root `_OSI` and `_REV`** are as G10 says. Decision item 309 already covers them.
- **Two limits on methods.** A method may be entered at most 255 times at once (`source/components/dispatcher/dsmethod.c` 340–347). A method's error may be turned into success by a host exception handler (`source/components/dispatcher/dsmethod.c` 212–239).

## 5. Files consulted

All files are from ACPICA, repository `open-acpica/acpica`, release `20260930` at commit `e85aa3bebaae81ed1e6d163511ac7e1eafe732cf`. All are under the BSD-3-Clause option of the `BSD-3-Clause OR GPL-2.0-only` licence, copyright Intel Corp. 1999–2026. All were read on 2026-11-19 (UTC) in the built-in browser, with nothing saved. This table feeds the "references consulted" field of `origin-aml`'s row in `docs/engineering/SOURCE_PROVENANCE.md`, and CLAUDE.md §6's similarity check before the code leaves the batch for `main` (decision item 121).

| # | File | Lines | Read for | What was read |
|---|---|---|---|---|
| — | GitHub releases and tags pages of `open-acpica/acpica` | — | The release | Newest release 20260930, its tag and commit |
| 1 | `source/include/acpixf.h` | 1,313 | Section 2; G2, G6, G10, G16 | Version; default modes |
| 2 | `source/include/acconfig.h` | 240 | G7, G10 | `_OS` string; `Sleep` cap; loop timeout |
| 3 | `source/include/amlcode.h` | 481 | G8 | Opcode values |
| 4 | `source/components/parser/psargs.c` | 1,162 | G1, G13 | Names in operands; string literals |
| 5 | `source/components/parser/psloop.c` | 575 | Section 2; G1, G3 | Parse loop and load-error policy |
| 6 | `source/components/parser/psobject.c` | 749 | Section 2; G1, G3, G8 | Unknown opcodes; completing a failed statement |
| 7 | `source/components/parser/psparse.c` | 723 | G2, G3, G6 | Method end; return values; mutex release |
| 8 | `source/components/parser/psxface.c` | 381 | Section 2; G6 | Running a method and a table |
| 9 | `source/components/namespace/nsparse.c` | 302 | Section 2; G1 | Table run as one method |
| 10 | `source/components/namespace/nsload.c` | 329 | Section 2; G1, G3, G17, G19 | Loading one table |
| 11 | `source/components/namespace/nsaccess.c` | 782 | G1, G10, G12 | Root objects; name lookup |
| 12 | `source/components/namespace/nssearch.c` | 424 | G3 | Existing names |
| 13 | `source/components/namespace/nsinit.c` | 740 | G1, G17 | Deferred packages; `_INI`, `_REG` at boot |
| 14 | `source/components/namespace/nsalloc.c` | 571 | G19 | Removing names |
| 15 | `source/components/dispatcher/dswstate.c` | 818 | G6, G12 | Result stack; method scope |
| 16 | `source/components/dispatcher/dsmethod.c` | 959 | G2, G3, G6, G19 | Serialization; method entry and end |
| 17 | `source/components/dispatcher/dsutils.c` | 918 | G6 | Implicit return; missing operand |
| 18 | `source/components/dispatcher/dswexec.c` | 791 | G3, G6, G9 | Named objects in methods; 32-bit truncation; predicates |
| 19 | `source/components/dispatcher/dswload.c` | 616 | G3 | Load passes; `External`; override |
| 20 | `source/components/dispatcher/dswload2.c` | 771 | G3 | Creating names; `Scope` targets |
| 21 | `source/components/dispatcher/dsobject.c` | 561 | G11 | Buffer and string objects |
| 22 | `source/components/dispatcher/dspkginit.c` | 565 | G1, G14 | Package elements |
| 23 | `source/components/dispatcher/dsinit.c` | 245 | G2, G17 | Per-table object initialisation |
| 24 | `source/components/dispatcher/dsopcode.c` | 813 | G17 | Region initialisation entry (lines 41–65 only) |
| 25 | `source/components/executer/exsystem.c` | 325 | G7 | `Stall`, `Sleep` |
| 26 | `source/components/executer/exoparg1.c` | 1,132 | G7, G8 | One-operand statements (lines 120–200 only) |
| 27 | `source/components/executer/exoparg2.c` | 613 | G9 | Math results (lines 255–300 only) |
| 28 | `source/components/executer/exmisc.c` | 644 | G9 | Math operators (lines 261–347 only) |
| 29 | `source/components/executer/exmutex.c` | 556 | G2 | `Acquire`, `Release`, forced release |
| 30 | `source/components/executer/exconvrt.c` | 787 | G4, G11, G15 | Conversions |
| 31 | `source/components/executer/exstore.c` | 631 | G11, G15 | Store to a named object (lines 369–580) |
| 32 | `source/components/executer/exstorob.c` | 211 | G11 | Buffer and string stores |
| 33 | `source/components/executer/exfield.c` | 466 | G15 | Field write |
| 34 | `source/components/executer/exfldio.c` | 1,019 | G5, G15, G16 | Field units and region checks |
| 35 | `source/components/executer/exprep.c` | 638 | G5, G16 | Access widths |
| 36 | `source/components/executer/exconfig.c` | 770 | G8, G17, G18 | `Load`, `LoadTable`, `Unload` |
| 37 | `source/components/tables/tbxfload.c` | 515 | Section 2; G3 | Boot loading of all tables (lines 105–254 only) |
| 38 | `source/components/tables/tbdata.c` | 1,300 | G17 | Loading one table (lines 1055–1110 only) |
| 39 | `source/components/tables/tbfind.c` | 148 | G18 | Table search |
| 40 | `source/components/events/evrgnini.c` | 694 | G17 | Region initialisation (lines 562–693 only) |
| 41 | `source/components/events/evregion.c` | 947 | G17 | `_REG` runs (lines 44–91 and 573–786 only) |
| 42 | `source/components/utilities/utdecode.c` | 664 | G12 | Scope-opening types (lines 23–60 only) |
| 43 | `source/components/utilities/uthex.c` | 127 | G4 | Hex digits (lines 14–60 only) |
| 44 | `source/components/utilities/utstrtoul64.c` | 365 | G4 | String to integer |
| 45 | `source/components/utilities/utstrsuppt.c` | 526 | G4 | Hex string and white space (lines 168–225 and 259–283 only) |
| 46 | `documents/changes.txt` (GitHub file page) | 19,995 | G1–G4, G6–G8, G10, G16, G17 | Change-log entries cited above, found by searching for the gaps' terms; not read whole |

Where only part of a file is named, only that part was read; elsewhere the parts cited above were read, after a search of the file for the gap's terms.

**Not read, and why.**
- ACPICA's reference manual and design documents (`documents/*.pdf`, `*.doc`): the code and the change log answered every gap.
- The code behind the change log's note on duplicate SSDTs (G3).
- How the parse levels around a failed statement are unwound in detail (G1, G3). This ledger states only that parsing goes on from where the failed statement stopped.
- No other project's source.
