# Origin Interface Definition Language (OIDL)

| | |
|---|---|
| Document | `docs/spec/OIDL.md` |
| Version | 1 |
| Status | **Accepted by the owner on 2026-11-09**: questions Q1–Q12 answered with the recommended option of each (CLAUDE.md §84). The grammar, the fixed-layout wire format and the handshake are decided. The generator exists, and its output is in the tree: the kernel's system calls (`framekernel/syscalls`, since task S3-0) and the service directory's interface (`sdk/directory`, task S3-4) |
| Requires | ADR-0011 N3, N4, N5, N7, N8, N9, N10, N13; ADR-0005 P1, P2, P4, P5, P8, P12, amendments A1–A6, B1, B2, B3; ONBF-R7, ONBF-R8; CLAUDE.md §3, §5, §10, §13, §30, §42, §45, §49, §62, §63, §84 |
| Implementation | `developer-tools/oidl` (host only). Consumers: `sdk/interfaces/origin.kernel.oidl` and `sdk/interfaces/origin.directory.oidl` |
| Evidence | `docs/research/2026-11-09-native-sdk.md`, `docs/research/2026-11-09-oidl-wire-format.md`, and the comparison with the hand-written ABI, `docs/engineering/OIDL_KERNEL_COMPARISON.md` |
| Compatibility | Every byte layout here is revision 1 of the format, accepted by the owner on 2026-11-09. A change to any of them needs the owner's acceptance again (CLAUDE.md §84) |
| Revisions | 2026-11-10: plain structs of integers, enums and bits as `syscall` vector and array elements (§3.1, §10), ADR-0005 C3, accepted by the owner on 2026-11-09 (decision sheet item 58). Existing definitions and the `channel` wire format are unchanged. 2026-11-10: the fingerprint names a library's own `sized struct` with its size at the minor described (§13), as §9 requires; the owner asked for the fix. 2026-11-10: from major 1, an item may not name one of its library's items from a later minor (§9). 2026-11-10: `--library`, one channel library's bindings as a crate of their own (§11) and a lock per library (§13), for the service directory; §7.4 records that `channel_read` reports handle records. Tools only: no format changes. 2026-11-12: a handle is a 64-bit value at the `syscall` transport, and the decoder no longer refuses a handle register whose upper 32 bits are not zero (§10): ADR-0005 Amendment 6, E1, accepted by the owner on 2026-11-10 (decision sheet item 109), built by task S4-0. The `channel` wire format is unchanged: a slot is a position in the message's handle list, not a handle value (§7.4). 2026-11-12: the canonical description is format `oidl-fingerprint 2`, which records a handle's width (§13): decision item 237, accepted by the owner on 2026-11-12, built by TASKS item 39; every lock recorded again. No byte layout changes |

The keywords MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119.

---

## 1. What OIDL is

OIDL is the single source of truth for every operation a program can ask the OS for (ADR-0011 N7). Each interface declares its methods, typed arguments, errors, version and the capability each method requires. A host tool, `oidl`, reads these definitions and generates, for each library: safe Rust clients and server traits, the C header, validating encoders and decoders, the kernel's call table and decoders, `origin.core`'s stubs, reference documentation, an ABI fingerprint and a hostile-message corpus.

OIDL has two transports, because ADR-0011 N8 has two binding costs that cross a boundary:

| Transport | Used for | Reaches its implementation through | Numbers |
|---|---|---|---|
| `syscall` | Everything the Framekernel offers | `origin.core`'s stubs, `syscall` (ADR-0005 P4) | Private to each release (§42) |
| `channel` | Every service | A channel with generated stubs (ADR-0005 P8) | Ordinals: part of the stable ABI |

A third binding, a plain library call inside the caller's process, needs no wire format and no decoder. It is not specified here (§18).

The kernel does not know OIDL. A channel carries bytes and handles; what they mean is an SDK layer above it (ADR-0005 P8).

---

## 2. Source files

A library is one UTF-8 text file. The tool refuses anything else, and it reads every input as untrusted (CLAUDE.md §13): limits are checked before work is done, arithmetic is checked, and nothing panics.

| Rule | Limit |
|---|---|
| File size | 1 MiB at most |
| Tokens | 200 000 at most |
| Line length | 4 096 bytes at most |
| Identifier, string | 64 bytes at most |
| Type nesting (`array<array<…>>`) | 8 levels at most |
| Struct nesting (a struct holding a struct …) | 16 levels at most |
| `use` lines in a library | 64 at most |
| Items in a library, members of an enum or bits, methods of an interface | 4 096 at most |
| Fields of a struct | 1 024 at most |
| Parameters, or results, of a method | 64 at most |
| Attributes on a declaration | 8 at most |

Every limit is checked before the work it guards. The struct-nesting limit is what bounds every recursive walk of a type, and the recursive drop of the model.

**Characters.** Outside comments only printable ASCII (`0x20`–`0x7E`) and line feed are allowed: no tab, no carriage return. Inside a comment any UTF-8 is allowed except control characters.

**Comments.** `//` to the end of the line is ignored. A line comment that begins with exactly three slashes is a **doc comment**: it belongs to the next declaration, and it may appear only before something that can carry documentation. Doc text is trimmed, and it MUST NOT contain `*/` or `/*`, because it is copied into C block comments, or a code fence (three backticks or three tildes), because it is copied into Rust doc comments and `rustdoc --test` would run a fenced block as code. Trimming keeps an indented line from becoming an indented code block for the same reason. Four or more slashes make an ordinary comment.

**Identifiers** are `[a-z][a-z0-9_]*`: lower-case only, no leading or trailing `_`, and no `__`, so that generated names are mechanical. The tool refuses a name that is a keyword of Rust, of C through C23, or of C++ through C++26, that begins with `oidl_`, or that would collide with another name once it is converted to `PascalCase` or `SHOUTY_CASE` (`foo_1` and `foo1` collide). A few names are reserved for what the generator emits: the `PascalCase` forms `Error`, `Handle`, `Result` and the like, the type words (`u8` … `i64`, `bool`, `handle`, `optional`, `array`, `vector`, `string`) for items, `types` for an interface, and `holds` and `transport` for a method. (A generated client's hello is `new`, already a C++ keyword. Until 2026-11-10 it was `connect`, which took that name from every interface, and C12's directory needs it.)

**Numbers** are decimal or `0x` hexadecimal, up to 64 bits, with `_` allowed between digits. A decimal number has no leading zero. A number is negated with a leading `-` only where a value is expected.

**Keywords** are contextual: they mean something only at the places the grammar expects them, so `handle` and `version` are valid parameter names.

### 2.1 Grammar

Every library has this shape, in this order. `Doc` is one or more doc comments. Declarations MAY refer to items declared later in the same library, because names are resolved after parsing.

```
File        = Doc "library" Dotted ";"
              "transport" ( "syscall" | "channel" ) ";"
              "version" ( Version | "compatibility" String ) ";"
              { "use" Dotted ";" }
              { Item }
Item        = Const | Enum | Bits | Struct | Capability | Interface
Dotted      = Ident { "." Ident }
Version     = Number "." Number

Const       = Doc { Attr } "const" Ident ":" Name "=" Value ";"
Value       = [ "-" ] Number | Ident { "|" Ident }

Enum        = Doc { Attr } "enum" Ident ":" Name "{" { Member } "}"
Member      = [ Doc ] { Attr } Ident "=" [ "-" ] Number ";"
Bits        = Doc { Attr } "bits" Ident ":" Name "{" { Member } "}"

Struct      = Doc { Attr } [ "sized" ] "struct" Ident "{" { Field } "}"
Field       = [ Doc ] { Attr } Ident ":" Type ";"

Capability  = Doc { Attr } "capability" Dotted ";"

Interface   = Doc "interface" Ident "{" { Method } "}"
Method      = Doc { Attr } [ "oneway" ] "method" Ident "=" Number
              "(" [ Params ] ")"
              [ "->" ( "never" | "(" [ Params ] ")" ) ]
              [ "errors" "(" [ Ident { "," Ident } [ "," ] ] ")" ]
              "requires" ( "none" | Dotted [ "else" Ident ] ) ";"
Params      = Param { "," Param } [ "," ]
Param       = [ Doc ] { Attr } Ident ":" Type

Attr        = "@" Ident [ "(" [ AttrArg { "," AttrArg } ] ")" ]
AttrArg     = Number "." Number | Dotted

Type        = "bool" | "handle" "<" HandleArgs ">"
            | "optional" "<" "handle" "<" HandleArgs ">" ">"
            | "array" "<" Type "," Bound ">"
            | "vector" "<" Type "," Bound ">"
            | "string" "<" Bound ">"
            | Name
HandleArgs  = Ident [ "," Ident { "|" Ident } ]
Bound       = Number | Name
Name        = Ident [ "." Ident ]
String      = '"' { printable ASCII except '"' and '\' } '"'
```

`Name` is an integer type (`u8` … `i64`), an item of this library, or `lib.item`, where `lib` is the last component of a `use`d library.

A library, a `const`, an `enum`, a `bits`, a `struct`, a `capability`, an `interface` and a `method` MUST carry a doc comment: the reference documentation is generated from them and may not have holes (CLAUDE.md §71).

---

## 3. Types

| Type | Size, alignment | Valid values |
|---|---|---|
| `bool` | 1, 1 | `0` or `1` and nothing else (§10) |
| `u8`, `u16`, `u32`, `u64`, `i8`, `i16`, `i32`, `i64` | width, width | all |
| `enum` | its integer | exactly the declared members |
| `bits` | its unsigned integer | exactly the combinations of the declared bits |
| `struct` | §5.3 | each field valid, padding zero |
| `array<T, N>` | `N × size(T)`, `align(T)` | each element valid; `1 ≤ N ≤ 65 536`, and the array is at most 64 KiB |
| `vector<T, N>` | §7.3; `T` is a fixed-size type or a `handle` | `count ≤ N`, `1 ≤ N` |
| `string<N>` | as `vector<u8, N>` | valid UTF-8, with no `NUL`, of at most `N` bytes |
| `handle<O, R>` | §7.4 | §6 |
| `optional<handle<O, R>>` | §7.4 | present or absent |

There is no floating-point type, union or nullable pointer in this revision. A later revision may add them where an interface needs one, as additions (§9).

### 3.1 Where each type may appear

| Type | Struct field, array element, vector element | Channel parameter or result | Syscall parameter | Syscall result |
|---|---|---|---|---|
| Scalar, enum, bits | yes | inline | a register | an out pointer |
| Plain struct, array of those | yes | inline | a pointer | an out pointer |
| `sized struct` | no | no | a pointer | a pointer (`size` set by the caller) |
| `vector`, `string` | no (no nesting) | count inline, bytes trailing | pointer and count | pointer and capacity, with `@count` |
| `handle` | vector element | slot inline | a register | an out pointer |
| `optional<handle>` | no | slot inline | a register | an out pointer |
| `array<handle>` | no | no | no (a `vector` states each handle's rights) | an out pointer |

Nesting is never variable-length, so no decoder recurses over message data, and no generated structure holds a pointer.

In a `syscall` library the elements of a `vector` or `array` are integers, handles, or plain structs whose fields are integers, enums and bits (ADR-0005 C3). Integers and handles cross as their in-memory form. A struct element crosses as its fields, each at its §5.3 offset with zero padding, and never in a compiler's layout (§42): the Rust client encodes and decodes each element field by field, the kernel's server reads each one as a fixed-size value, and in C each element is the generated struct, whose layout the header's assertions pin to the same bytes. A `bool`, a nested struct, an array or a handle inside the element is refused. A vector of structs is at most 64 KiB at its bound, because the client encodes it into a buffer of that size.

---

## 4. Declarations

### 4.1 Constants

`const NAME: TYPE = VALUE;` declares a named integer or `bits` value: the value is checked against the type. A `vector` or `string` bound may name an unsigned constant.

### 4.2 Enumerations and bits

An `enum` is closed: a decoder refuses any value not declared, in every position. Its values are explicit and unique. A `bits` member is a **bit index** (`read = 0` is `1 << 0`), and every undeclared bit is refused. `@nonzero` on a parameter of `bits` or integer type also refuses zero.

### 4.3 Three reserved roles

Three names are special, and each MUST be declared at most once in a compilation:

| Name | Must be | Is used for |
|---|---|---|
| `status` | an `enum` over `i64` with `ok = 0` and every other member negative, ADR-0005 P5 | every error |
| `rights` | a `bits` over `u32` | the rights half of a `handle` type |
| `object_type` | an `enum` over `u32` with no member named `any` and none with the value 0, which means "any" | the object half of a `handle` type |

Two constants are also roles: `max_message_bytes` and `max_message_handles`, the limits of ADR-0005 P8. A `channel` library MUST have access to all of these through `use`. By convention they are all declared by `origin.kernel`.

### 4.4 Attributes

| Attribute | On | Means |
|---|---|---|
| `@since(M.m)` | a method; a parameter or result of a `channel` method; an enum or bits member; a field of a `sized struct`; a `const`, `enum`, `bits`, `struct` or `capability` | Added in minor `m` of the current major (§9). A member or field may not be older than the item that holds it, and a parameter may not be older than its method |
| `@nonzero` | a parameter of integer or `bits` type | Zero is refused |
| `@count(p.f)` | a `vector` or `string` result of a `syscall` method | The filled length is the `u32` field `f`, of the struct result `p`, from that struct's base version |
| `@also_on(e, …)` | a result of a `syscall` method, other than a `vector` or `string`, which a failure does not report the length of | This result is also written when the method fails with one of these declared errors |
| `@reserved` | a method of a `syscall` interface | The number and signature are reserved, and no implementation exists: the generated dispatcher answers `not_supported`. The method MUST declare `not_supported` |

An attribute anywhere else is refused.

### 4.5 Methods

```oidl
/// Duplicates a handle, keeping at most the rights it has.
method handle_duplicate = 4 (
    handle: handle<any, duplicate>,
    rights: rights,
) -> (new_handle: handle<any>)
    errors(bad_handle, invalid_args, fault, access_denied, limit, no_memory)
    requires none;
```

- The number after `=` is the method's **ordinal**. It is explicit, so reordering declarations changes nothing. It is unique in its interface (`channel`) or in its library (`syscall`).
- `-> never` is allowed only in a `syscall` interface: the call does not return (`thread_exit`). Its parameters are `u64`, because a call that cannot fail cannot refuse a bad argument.
- `oneway` is allowed only in a `channel` interface: there is no reply, so there is no result and no `errors` clause.
- The `errors` clause is mandatory on every other method. `errors()` means it cannot fail.
- `requires` is mandatory on every method (§6).
- A parameter name is unique among the parameters and results of a method.

---

## 5. Layout of fixed-size types

### 5.1 Encoding

All integers are little-endian. A `bool` is one byte.

### 5.2 Padding

Padding is never free space. Every padding byte, wherever it is, MUST be written as zero and MUST be refused by a decoder if it is not zero (ADR-0011 N7). The C header makes padding visible as named members so that a C caller that initializes a structure with `{0}` writes it correctly.

### 5.3 Structures

Fields are placed in declaration order, each at the next offset that is a multiple of its alignment. The structure's alignment is its largest field alignment. Its size is rounded up to a multiple of that.

A plain `struct` is frozen: its layout never changes within a major, and none of its fields may have `@since`.

A `sized struct` begins with an implicit `u32 size` at offset 0, and it is the only structure that can grow (ADR-0005 P4). A field added later carries `@since`. Declaration order MUST be non-decreasing in version. Each version's fields start at or after the size of the previous version, so **each version's layout is a prefix of the next**. A `sized struct` is allowed only in a `syscall` library, and only at the top level of a parameter or result: it is never nested, because a nested structure cannot grow.

How `size` is checked:

| Direction | Rule |
|---|---|
| In a parameter | `size` MUST be exactly the size of one version this side knows, otherwise `invalid_args`. Fields of later versions read as absent (Rust `None`) |
| In a result (`size` set by the caller) | `size` MUST be at least the size of the base version, otherwise `invalid_args`. The server writes only the fields it knows that fit in `size`. A larger `size` is allowed, and the server does not touch what lies beyond its own fields |

These two rules are P4 and A4 of ADR-0005 applied in each direction. The parameter rule is exact because a `size` between two versions cannot come from a correct caller, and a larger one cannot either: the Loader Service refuses an image built for a newer minor (P12). The result rule is the kernel's behaviour today: a caller built against a newer minor may pass a larger structure, and an older kernel writes only what it knows.

---

## 6. Declared capabilities

Authority is declared in two places, and the stub enforces both before any implementation runs (ADR-0011 N9).

**Rights on handles.** `handle<O, R>`: `O` is a member of `object_type` or the word `any`; `R` is a `|`-separated list of members of `rights`. A received or passed handle MUST name an object of type `O` and hold every right in `R`. `handle<O>` needs no rights. A decoder cannot see rights in the message: for the `channel` transport the receiver is given each handle's object type and rights by the transport (§7.4), and for the `syscall` transport the kernel's handle table enforces them.

**Named capabilities.** `capability x.y;` declares a capability, in the dotted style of ONBF's manifest capabilities (`codegen.jit`, `cit.grant`). A method says either `requires x.y` or `requires none`. The clause is mandatory, so the choice is always visible at the method. It answers whether this caller may use the method at all, and it never crosses the wire: the stub asks a predicate supplied by the implementation (`holds`), which for a service is what the launcher granted on this connection (ADR-0011 N9), and for the kernel is the process object's state.

A method whose capability is not held is refused before its arguments are decoded. In a `syscall` interface the refusal status is `access_denied`, or the status named by `else` (`requires x.y else not_supported`, which exists to keep the developer-only calls' existing status), and that status MUST be in the method's `errors`. In a `channel` interface the refusal is always `access_denied` (§7.8), it is not listed in `errors`, and `else` is not allowed. A `never` method cannot require a capability: it has no way to report a refusal.

---

## 7. The `channel` wire format, revision 1

A message is what ADR-0005 P8 calls a message: at most `max_message_bytes` bytes and `max_message_handles` handles, read and written as a unit. Everything below is checked by the receiver. Validation is the same in every generated decoder and it happens in the order given: an implementation that orders its checks differently is non-conforming, because the corpus (§14) pins which check fires first.

### 7.1 Header

Every message starts with a 16-byte header, aligned on 8 bytes.

| Offset | Field | Value |
|---|---|---|
| 0 | `u32 size` | Total bytes of the message, header included. MUST equal the message's length, which MUST be a multiple of 8 |
| 4 | `u8 wire` | The format revision. This specification is `1` |
| 5 | `u8 kind` | `1` request, `2` response, `3` error |
| 6 | `u16 reserved` | Zero |
| 8 | `u32 ordinal` | The method's ordinal. Ordinal `0` is the hello (§7.7). Values above `0x7FFF_FFFF` are reserved |
| 12 | `u32 txid` | Non-zero for a request that expects a reply and for its reply. Zero for a `oneway` request |

The `size` first follows ADR-0005 P4. It also lets a reader frame a message before it knows the method.

### 7.2 Body

```
body = inline ‖ variable
```

**Inline.** Parameters are placed in declaration order, as the fields of a structure are (§5.3). Before the first parameter of a later `@since` version, the offset is rounded up to 8, and the inline part is rounded up to a multiple of 8 at the end. Each version's inline part is therefore a prefix of the next.

| Parameter | Inline form |
|---|---|
| Scalar, enum, bits | Its bytes (§5) |
| Struct, array | Its bytes (§5) |
| `vector`, `string` | `u32 count` |
| `handle` | `u32 slot` (§7.4) |
| `optional<handle>` | `u32 slot` (§7.4) |

**Variable.** After the inline part come the vectors and strings, in declaration order. Each one's elements are packed at their element size, then padded to a multiple of 8 with zero bytes.

An empty vector occupies nothing. Because every length is fixed by a `count` in the inline part, there are no offsets or pointers to validate: a message's layout is entirely determined by the definition and its counts, and exactly one byte sequence encodes any value.

### 7.3 Exact size

For a body at negotiated minor `m`:

```
expected = inline_size(m) + Σ round_up_8(count_i × element_size_i)
```

The body's length MUST equal `expected` exactly. A shorter or longer body is refused. Unknown trailing bytes are never tolerated: a newer peer that wants to add a field uses a `@since` parameter, and the negotiation in §7.7 tells both sides exactly which layout is in use. This is how "exact sizes" (ADR-0011 N7) and "a newer minor can append fields" (ADR-0005 P4) are both true.

### 7.4 Handles

Handles are out of band: the kernel moves them with the message, in the message's handle list (ADR-0005 P8), and no handle value appears in the bytes. The body refers to them by position.

- A `handle` occupies a 4-byte **slot** holding the zero-based index of the handle in the list.
- An `optional<handle>` slot holds its index, or `0xFFFF_FFFF` if absent.
- A `vector<handle<…>, N>` has `count` slots in the variable part.

Handles are consumed in field order, starting from 0. So every present slot is exactly determined: the `k`th present slot MUST equal `k`. When the message has been read, the number of handles consumed MUST equal the length of the list. There is no way to name a handle twice, to skip one, or to leave one out.

The transport supplies, for each handle in the list, its value, its object type and its rights. A slot's handle MUST match the `object_type` and every right its declared type names (§6). `channel_read` reports all three as a `received_handle` record per handle, from `origin.kernel` 0.2 (Q8, ADR-0005 B3, built 2026-11-10), each made from what the reader's table holds, so this rule holds on a real channel.

### 7.5 Order of validation

A receiver checks, stopping at the first failure:

1. The header: its length, `size` (equal to the length, a multiple of 8), `wire`, `kind` (a known value), `reserved`.
2. The kind is the one expected here (a request, or a response), and the `txid` rule.
3. The ordinal is known at the negotiated minor.
4. The capability (§6).
5. The inline part is entirely present.
6. Each inline field, by ascending offset: padding, `bool`, `enum`, `bits`, `@nonzero`, vector `count ≤ N`, handle slot, then that handle's object type and rights.
7. The body's exact size (§7.3).
8. Each vector and string in order: elements (as 6), UTF-8, no `NUL`, slots, padding.
9. Every handle has been consumed.

### 7.6 Error replies

A method that fails replies with `kind = 3` and the request's ordinal and `txid`. Its body is exactly one negative `i64 status`. No other field is sent, and it carries no handles.

### 7.7 The hello

A channel has no connection step, so the first message on every connection is a **hello**, and a server accepts nothing else first.

Its request has `ordinal = 0`, a non-zero `txid`, no handles, and a 16-byte body. Its response is the same shape.

| Offset | Field | Value |
|---|---|---|
| 0 | `u64 interface_id` | The first 8 bytes, as a little-endian integer, of `SHA-256("origin.oidl.interface.v1\0" ‖ "library.interface")` |
| 8 | `u16 major` | The sender's major version |
| 10 | `u16 minor` | Request: the client's highest minor. Response: the negotiated minor, `min(client, server)` |
| 12 | `u32 reserved` | Zero |

The server replies with `not_supported` (kind 3) if `interface_id` is not its own or if `major` differs, and it does not negotiate (ADR-0011 N10). A hello that arrives after negotiation is refused. A client checks that the reply is a response on ordinal 0 with its own `txid`, that it carries no handles, that it names the interface and major it asked for, and that the minor is no higher than the one it offered. After the hello, both sides use the layout of the negotiated minor for the whole connection, so a decoder refuses anything that does not exactly match that minor.

### 7.8 How the receiver refuses

"Refuse" never reaches service code (ADR-0011 N9). Every handle delivered with a refused message is closed by the generated stub's caller: a client cannot run a server out of handles by sending invalid messages.

| Failure | Action |
|---|---|
| The header is invalid, or the kind or `txid` rule fails | Drop, and tell the caller to close the connection; the service has taken no handles |
| Any message other than a hello before negotiation; a hello after a successful one; a hello that is malformed or carries handles | The same |
| A hello that names another interface or major | Error reply: `not_supported`. The connection stays un-negotiated, so a client may try another interface or major |
| Unknown ordinal, or a method newer than the negotiated minor | Error reply: `not_supported` (`oneway`: drop) |
| Capability not held | Error reply: `access_denied` (`oneway`: drop) |
| Any failure in the body | Error reply: `invalid_args` (`oneway`: drop) |
| A reply the implementation cannot encode (more than its bound) | Close the connection: a defect of the service. It has already run, so it keeps the handles it took, and the handles it put in its reply stay in its own table |

These three statuses are **refusals**. They are not listed in a method's `errors`, and a client sees them separately from the method's own errors. The reply's size is fixed and it carries no handles, so a refusal does not amplify anything a client sends.

Each outcome says whether the service took the received handles. If it did not, the caller of the stub closes them. A client does the same for a reply it refuses: every handle delivered with a reply that fails validation, an error reply (which may carry none) or a hello reply (which may carry none) is closed, through the transport.

### 7.9 A worked example

For a method `put = 5 (flag: bool, count: u16, name: string<16>, h: handle<channel, read>)`, with `flag = true`, `count = 0x0102`, `name = "hi"` and one handle, a two-way request with `txid = 7` is these 40 bytes, and the handle goes in the message's handle list:

```
28 00 00 00   size = 40
01            wire
01            kind = request
00 00         reserved
05 00 00 00   ordinal
07 00 00 00   txid
01            flag
00            padding
02 01         count
02 00 00 00   name's count
00 00 00 00   slot: handle 0
00 00 00 00   padding, to 8
68 69 00 00 00 00 00 00   "hi", padded to 8
```

`tests/programs/fixture_api.rs` pins these bytes against the generated encoder and decoder, and `src/tests/model.rs` pins them against the independent model of §14.

---

## 8. Errors

A method declares its failures as a subset of the single `status` enumeration (ADR-0005 P5). The generator produces a closed Rust enum for each method, so an implementation cannot return an undeclared status: that is a compile error, not a runtime check. A client decodes a status into one of four cases:

| Case | Meaning |
|---|---|
| The method's own error | A status in its `errors` |
| A refusal | `invalid_args`, `not_supported` or `access_denied`, on a `channel` method that did not declare it |
| Undeclared | Any other status: a broken peer, reported distinctly |
| Transport | The channel or the system call itself failed |

A method's error set is frozen for the whole major version: it is not an "addition" that old clients can ignore. A new failure mode gets a new method.

---

## 9. Versions

A library declares one `version M.m`, and each of its interfaces has that version (ADR-0011 N3: the promise is per domain). `M` and `m` are 16-bit. The version is an ABI identifier, separate from the release (§63). A library whose version is already an identifier in `origin-version.toml` names it instead of copying it, so that the number exists in one place: `version compatibility "user-abi";` (§62).

Within a major version only additions are allowed (ADR-0011 N10):

| Allowed in a new minor (`@since`) | Forbidden within a major |
|---|---|
| A new method | Removing or renaming anything |
| A new `enum` or `bits` member | Changing a type, ordinal, value, bound or `requires` |
| A new parameter or result of a `channel` method, placed last | Changing a method's errors |
| A new field at the end of a `sized struct` | Changing a plain `struct` |
| A new `const`, `struct`, or capability | Reordering |

`@since(M.m)` names the current major, with `1 ≤ m ≤` the current minor. An item without it dates from `M.0`. Bumping the major therefore forces every `@since` to be removed.

From major 1, an item may not name an item of its own library that a later minor added: a parameter or result its type (or the element type inside it), a field its type, a constant its bits type or the members it combines, a method its capability or its statuses. No version of the definition has both, so the reference could not be kept compatibly, and the tool refuses it (`NewerReference`). At major 0 it is allowed, and the fingerprint gate reports the minors it changes instead: `origin.kernel`'s `channel_read`, from minor 0, names `received_handle`, from minor 2 (ADR-0005 B3). Decided 2026-11-10 with the fingerprint fix (§13); items of another library are versioned by that library, so they are outside this rule.

Major `0` promises nothing, like ADR-0005 P12's `user-abi` today: a change at major 0 is reported and not refused.

A later-version parameter of a `channel` method is `Option<T>` in Rust: `None` when the negotiated minor predates it. An enum or bits member newer than the negotiated minor is refused.

Minors are per library, and only the library that owns the interface is negotiated. So an enum or bits type from a `use`d library is read with all of its members known: its members cannot be version-gated by a number that belongs to another library. A newer peer sending a member an older definition does not know is refused all the same, by the closed enum.

---

## 10. The `syscall` transport

Registers and pointers follow ADR-0005 P4: the number in RAX, up to six arguments in RDI, RSI, RDX, R10, R8, R9, and RAX returns a status. **Call numbers are private to the release** (§42): they appear only in `origin.core`'s stubs and in the kernel's dispatcher, and they are excluded from the stable fingerprint (§13).

Parameters take registers first, in declaration order, then results, in declaration order:

| Type | As a parameter | As a result |
|---|---|---|
| `handle` | one register | one out pointer (`u64`) |
| `optional<handle>` | one register; `0` is absent | one out pointer |
| `bool`, integer, enum, bits | one register | one out pointer |
| struct, `array` | one pointer | one out pointer |
| `vector`, `string` | pointer, then count | pointer, then capacity (`@count` is required) |

A vector's count and a result's capacity are in elements. A struct element is its bytes as §3.1 lays them out, so the kernel's length in bytes is the count times the struct's §5.3 size.

**A handle is a 64-bit value** (ADR-0005 Amendment 6, E1). It fills its register as a parameter, and in memory it is a little-endian `u64`: a handle result's out pointer, each element of an `array<handle>` result and of a `vector<handle>` parameter, and a handle field of a structure the kernel writes, such as `received_handle` and `start_entry`, are 8 bytes. In C it is `origin_handle_t`, a `uint64_t`. It stays opaque (ADR-0011 N9): how the kernel divides its bits is not part of the ABI, and no program may compute one handle from another. A slot of the `channel` wire format (§7.4) is a position, not a handle, and stays 4 bytes.

A method that needs more than six registers is refused by the generator; ADR-0005 P4 says such a call takes a pointer to a structure.

**Register decoding** happens after the `@reserved` check (which answers `not_supported`) and the capability check (§6), in declaration order, the first failure winning:

| Type | Check | Refused with |
|---|---|---|
| `handle`, `optional<handle>` | None: every value goes to the kernel's table, which answers `bad_handle` for one that names nothing (ADR-0005 E1; until `origin.kernel` 0.7 the upper 32 bits had to be zero) | — |
| `u8`, `u16`, `u32` | The value fits | `invalid_args` |
| `i8`, `i16`, `i32` | The value is correctly sign-extended | `invalid_args` |
| `bool` | `0` or `1` (§10 of CLAUDE.md) | `invalid_args` |
| enum | A declared member | `invalid_args` |
| bits | No undeclared bit; `@nonzero` | `invalid_args` |
| `u64`, `i64` | None | — |
| `vector`, `string` parameter | `count ≤ N` | `invalid_args` |
| `vector`, `string` result | A capacity above `N` is clamped to `N`: more can never be needed | — |

A `string` parameter's content cannot be checked here: the decoder reads no user memory. The kernel validates it after its copy, with the generated `check_string` (valid UTF-8, no `NUL`).

Pointers are not dereferenced by the decoder. It produces typed descriptors of user memory; the kernel checks them against the calling process's tables and copies them once (ADR-0005 P4), as it does today.

Unused argument registers are ignored: a C stub leaves them undefined.

Every method MUST declare the statuses its own decoding can produce: `bad_handle` with a handle parameter (since E1 its decoding refuses no handle, but the table it is looked up in does, so the rule stands), `invalid_args` with a parameter narrower than 64 bits, a `bool`, an enum, a `bits` or a bounded count, `fault` with any pointer, `access_denied` with a handle that has rights, and `wrong_type` with a handle of a specific object type. A `@reserved` method is exempt, since it decodes nothing. The tool refuses a definition that leaves one out.

The generated C entry point is the stable interface: `<library with _>_<method>`, returning the status as `int64_t` (`_Noreturn void` for `never`). `origin.core` issues the `syscall` instruction inside each entry point, with **one `syscall` instruction per number**, as ADR-0005 B2 requires.

---

## 11. Generated output

`oidl generate` writes, with no timestamps, paths or other environment input (CLAUDE.md §75):

| Path | What | Unsafe |
|---|---|---|
| `rust/lib.rs`, `rust/oidl_rt.rs` | The crate root and the runtime (`Reader`, `Writer`, `Fixed`, `CallError`) | None |
| `rust/<lib>/types.rs` | Constants, enums, bits and structures, with validated encoders and decoders | None |
| `rust/<lib>/<interface>/server.rs` | Syscall: the call table, per-method errors, `Calls` and `dispatch`. Channel: request and response codecs, per-method errors, the server trait and dispatcher | None |
| `rust/<lib>/<interface>/client.rs` | The safe client | None |
| `rust/<lib>/<interface>/abi.rs` | Syscall: C-ABI imports and their thin wrappers | **The named shim** |
| `rust/<lib>/<interface>/core_stubs.rs` | Syscall: `origin.core`'s entry points; `x86_64` only | **The named shim** |
| `c/<lib path>.h` | The C header: `typedef`s, constants, structures with named padding and `_Static_assert`s, and for `syscall` the entry points | — |
| `doc/<lib>.md` | Reference documentation | — |
| `abi/<lib>.fingerprint` | The fingerprint (§13) | — |
| `corpus/<lib>.corpus` | The hostile corpus (§14) | — |
| `rust/bin/corpus_check.rs` | A program that runs the corpus against the generated code | — |

Generated Rust is `no_std`, allocates nothing and passes the project's no-panic lints (`clippy::panic`, `unwrap_used`, `expect_used`, `indexing_slicing`, `arithmetic_side_effects`, `cast_possible_truncation`) in every non-test build. It contains no `unsafe` outside the two named shims (ADR-0011 S7). No Rust layout crosses a boundary: every structure is encoded and decoded field by field (ONBF-R7, CLAUDE.md §42), and a `bool` crossing the C ABI is `uint8_t`.

Channel-transport C entry points are **not generated in this revision**: they would sit on `origin.core`'s call helper (ADR-0005 P8), which does not exist. The header carries their types and message sizes.

`oidl generate --server-only` writes one crate at the top of the output directory, and nothing else: `lib.rs`, `oidl_rt.rs`, `<lib>/types.rs` and each interface's `server.rs`. There is no client, C, documentation or corpus, and so no shim: the crate's root is `#![forbid(unsafe_code)]`. The files it shares with the full output are identical to it. Its modules are marked `#[rustfmt::skip]`, because what the tool wrote is what a repository commits and compares. The Framekernel's crate is made this way (`framekernel/syscalls`, ADR-0011 N14).

`oidl generate --library NAME` writes one crate for the `channel` library `NAME`, for its services and their clients, again at the top of the output directory and nothing else: `lib.rs`, `oidl_rt.rs`, `NAME`'s `types.rs`, and each of its interfaces' `server.rs` and `client.rs`; of the libraries it uses, directly or not, only their `types.rs`, each under a `mod.rs` that declares nothing else. A channel client needs no shim, so this crate has no `unsafe` either, and its root is `#![forbid(unsafe_code)]`. Every file but `lib.rs` and the used libraries' `mod.rs` is identical to the full output's. The tool refuses a `NAME` that is not among the inputs, and a `syscall` library, whose client is the shims. The service directory's crate is made this way (`sdk/directory`, from `origin.directory`).

---

## 12. The generator is a parser of untrusted input

The tool is written to the same rules as ONBF's parsers: `#![forbid(unsafe_code)]`, the no-panic lints in non-test builds, every limit in §2 checked, no recursion on untrusted depth, and every rejection a named error with a position. A generated decoder is a straight-line sequence of checks. It has no recursion, no loops except over a validated count, and no allocation.

---

## 13. The fingerprint and the compatibility gate

For each library and each minor `m` from 0 to its current minor, the tool renders the **canonical description**: UTF-8 text with one fact per line, covering exactly the items with `since ≤ M.m`. The fingerprint of that minor is `SHA-256` of the text.

- Comments and documentation never appear in it.
- Declaration order does not matter except where it is ABI: fields and parameters keep their order, everything else is sorted by name, ordinal or value.
- `syscall` numbers are excluded from the **stable** fingerprint (§42). A second fingerprint, the **release** fingerprint, includes them, so a build can check that the kernel and `origin.core` were generated from the same definition.
- The description starts with its own format version, `oidl-fingerprint 2`, followed by `library <name>`, `major <M>`, `transport <t>` and `handle <bits>`. A library's minor is not in it, so the digest of minor `m` does not change when a later minor is added.
- `handle <bits>` is the width of a handle's value: 64 bits, since ADR-0005 E1 (§10). No definition declares it. This specification sets it, the generator emits handles of that width from the same constant (`layout::HANDLE_BITS`), and every library's description records it at every minor. So a change of a handle's width changes every digest of every library, and no lock can stay as it was through one; a test plants another width and shows that it does (`a_planted_handle_width_changes_every_digest`). Decided by the owner on 2026-11-12 (decision item 237), built by TASKS item 39. Format 1 had no `handle` line: S4-0's change from 32 to 64 bits reached `origin.kernel`'s lock only through `received_handle` and `start_entry`, and nothing would have shown it in a library without them. Every lock was recorded again in format 2 while every library was at major 0, where a changed digest is reported and not refused (§9). Since the width is the specification's and not a library's, every minor's description carries today's width: a minor recorded before a width changed is described again with the new one, as any change at major 0 is.
- The rest is one line per fact, in this order, each rendered in `src/fingerprint.rs`: constants; enumerations with their members; bits with their members; structures with their fields and offsets; capabilities; then every method, by interface and ordinal, with its parameters, results, sorted error names and requirement. A test pins one digest (`the_fingerprint_is_pinned`).
- Items carry their `since`, and an item with `since` above `M.m` does not appear at minor `m`, so a `const`, `struct` or the like added in a later minor changes no earlier minor's fingerprint. An item added later without `@since` does change them: it is part of the earlier minors, as far as the definition says.
- A parameter or result that is a `sized struct` of the library names it with its size at minor `m`, so a field the struct gains later, an addition §9 allows, changes no earlier minor's fingerprint either. (Until 2026-11-10 the reference carried the newest size, and growing `read_sizes` changed `origin.kernel`'s 0.0 and 0.1: a warning at major 0, a refusal at any other.) A `sized struct` from another library, which nothing uses yet, is still expanded at its newest layout; whether its later fields are that library's own business, as an enum's later members are, is not decided.
- A type from another library is expanded in the description (the layout of a struct, the base members of an enum or bits): a used library's change of layout would otherwise alter this library's messages unseen.

A lock file records `<library> <M.m> sha256:<hex>`, one line per minor. `oidl fingerprint --library NAME` prints, checks and records `NAME` alone, the other inputs being there only so that it compiles, so each library can have a lock of its own. `oidl fingerprint --lock FILE` fails if, for the current major, a recorded minor's fingerprint has changed or a recorded minor has gone; if a recorded major is higher than the library's (the version went backwards); if the lock names a library that is not among the inputs; if a line is malformed; or if the file is missing and `--update` was not given. A minor that is not recorded yet is reported. At major 0 a changed or removed minor is a warning (§9). `--update` records new minors and refuses to run while there are errors.

Each minor's fingerprint is frozen once it is recorded, which is what makes additions compatible and everything else detectable (ADR-0011 N13). `cargo xtask check` is stricter than the tool: it fails if re-recording a repository's lock would change it, even at major 0, so that a change to an ABI is always recorded in the commit that makes it (`developer-tools/xtask/src/interfaces.rs`).

---

## 14. The hostile corpus

For each method the tool synthesizes a valid request and response at every minor, then derives hostile cases: every single byte replaced by each of `00`, `01`, `02`, `7F`, `80`, `FF` and its own value with bit 0 flipped; every declared member of every enum field and every declared bit of every `bits` field, whatever minor it arrived in, an undeclared bit, and zero for a `@nonzero` field; every truncation; extensions; handle lists one short and one long; each handle with the wrong object type and the wrong rights. Length changes re-seal the header's `size` so the checks behind it are reached.

The corpus is bounded whatever the definition asks for: a message above 1 KiB has only its first and last 512 positions mutated, a method covers at most 8 minors (each one a layout or a type changes at), and a corpus stops growing at 64 MiB, marking that in its last line.

Each case records the verdict and reason from an **independent model** of this specification (`wire.rs`, a table walker that shares nothing with the generated decoders). `syscall` methods get a corpus of register tuples at the edges of each argument.

The file is line-oriented text, stable under the deterministic generator:

```
# oidl-corpus 1 <library>
syscall <interface> <method> <number> holds=<0|1> args=<a0>,<a1>,<a2>,<a3>,<a4>,<a5> expect=ok
syscall <interface> <method> <number> holds=<0|1> args=… expect=<status name, lower case>
message <interface> <method> <request|response> minor=<m> handles=<v:o:r,…> bytes=<hex> expect=ok
message <interface> <method> <request|response> minor=<m> handles=… bytes=<hex> expect=<reason>
```

The reasons are `size`, `wire`, `kind` (an unknown kind), `direction` (a kind that is valid but not the one expected here), `reserved`, `txid`, `ordinal`, `padding`, `bool`, `enum`, `bits`, `nonzero`, `count`, `utf8`, `nul`, `handle_count`, `handle_slot`, `handle_type` and `handle_rights`.

Every generated decoder MUST reproduce the verdict and reason of every line. `rust/bin/corpus_check.rs` proves that for the generated code, and a set of planted faults proves the corpus would have caught each check being removed (§17).

---

## 15. Threat model (CLAUDE.md §58)

| # | Threat | Defence |
|---|---|---|
| O1 | A malformed message reaches a service | Generated validation before service code (§7.5, §7.8); exact size; closed enums and bits |
| O2 | One logical value has several encodings, so a signature or comparison over bytes is unsafe, or a filter and a service disagree | Canonical by construction (§7.2): zero padding, determined slots, exact size |
| O3 | A hostile count or offset drives an out-of-bounds read, or a huge allocation | No offsets; counts are checked against a declared bound and the exact size before any data is read; decoders do not allocate |
| O4 | Deep nesting exhausts a stack | No nesting of variable data; no recursion on message data |
| O5 | A handle is forged by a number in a message | Handles are out of band and moved by the kernel; a slot is an index that the decoder verifies, not a value |
| O6 | A handle has the wrong type or too few rights | Checked before service code (§6), given the kernel's report (Q8) |
| O7 | Handles are leaked by refused messages | The stub's caller closes all of them, on the server and on the client (§7.8) |
| O8 | A newer peer's field is silently ignored | Exact size at the negotiated minor (§7.3); newer enum and bits members refused |
| O9 | Downgrade or wrong service | Hello: interface id and major refused on mismatch (§7.7). Identity of the peer is ADR-0005 B1, and is not provided by the id (ADR-0011 S9) |
| O10 | A defect in the generator is repeated everywhere | Mutation-soaked parser; a hostile corpus labelled by an independent model; planted faults proving the corpus catches them; generated code compiled and run under the pinned toolchain (ADR-0011 S7) |
| O11 | A doc comment or name injects code into generated text | Names are restricted to a safe alphabet; doc text cannot close a C comment, cannot hold a code fence, and is trimmed, so `rustdoc --test` finds nothing to run; generated Markdown escapes doc text |
| O12 | An implementation returns a status the method does not declare | Compile error: the return type is a closed per-method enum (§8) |

---

## 16. Alternatives

| Alternative | For | Against |
|---|---|---|
| Pointer-based, out-of-line records, as in FIDL's format | A mature model | Offsets and presence markers must be validated; recursion needs a depth limit; Origin's first interfaces do not need nesting (§7.2) |
| Cap'n Proto's segments and pointers | Zero-copy, usable in place | More than one valid encoding of a value, and pointer validation (§15 O2, O3); a large format to re-implement (§3, §7) |
| FlatBuffers vtables | Field presence needs no version negotiation | Offsets to validate, more than one layout for a value, and ADR-0011 chose negotiation at connection time |
| OCBOR-1 | Already built and tested | Self-describing and slower; ADR-0011 keeps it for signed metadata and files |
| Handle slots with a present/absent marker instead of the index | Saves the index comparison | An in-place reader must count earlier handles; index equality makes every present slot fully determined (§7.4) |
| No in-band representation of handles | 4 bytes saved per handle | The same, and optional handles still need a presence bit |
| Accept unknown trailing bytes | Old receivers tolerate newer senders with no negotiation | A silently ignored field is a security decision taken by accident (§15 O8). Negotiation is already required by ADR-0011 N10 |
| Errors extendable per minor | Easier evolution | Old clients have no way to handle a status they were never told about (§8) |

`docs/research/2026-11-09-oidl-wire-format.md` records what was read and how.

---

## 17. Conformance

An implementation of this specification MUST: pass the malformed-definition suite; reproduce every line of the corpus; generate deterministic output; generate code that contains no `unsafe` outside the two named shims; compile under the pinned toolchain for the host, `x86_64-unknown-none` and `x86_64-unknown-linux-gnu` (§11 of CLAUDE.md); and produce a C header that compiles with no warnings under every published standard it can: C11 (the first with `_Static_assert`), C17 and C23, and C++11, C++17, C++20 and C++23. Host tests of `developer-tools/oidl` check all of these, and each check in the generated decoders has a planted fault that the corpus detects (`SOURCE_PROVENANCE.md`).

---

## 18. Not decided here

- How a plain library call (ADR-0011 N8's first row) is declared.
- Events: a server-initiated message.
- Floating-point types, unions, nested variable-length data.
- C entry points for the `channel` transport (§11).
- Where a `channel` stub puts ADR-0005 B1's stamped identity. Its fields (C1) and how a read delivers them, through `origin.kernel`'s `read_sizes` (C2), are decided and built (task S3-2, 2026-11-10); the generated server takes an opaque `Caller` until the first ring-3 service (ADR-0011 N14 step 4).
- Per-number pinning, which needs the number-to-address table the linker produces, and generation of the kernel's `syscall` table from this definition: the kernel's switch to generated code, which ADR-0011 N14 orders after this.
- The Driver SDK (ADR-0001 D2): its own ADR.

---

## 19. Questions for the owner (all answered 2026-11-09)

**The owner answered all twelve on 2026-11-09 with the recommended option of each.** Each recommendation below was checked against the whole of CLAUDE.md, including the header (x86-64 only, 2026-or-later hardware, M1 Max host), the priorities P0–P9 and the 40 final rules. The sections each one touches are named. Where a question is settled by a later slice, it says so.

| # | Question | Recommendation | Why (checked against) |
|---|---|---|---|
| Q1, answered | Accept wire format revision 1 as specified in §7: a 16-byte header with `size` first, an 8-aligned inline part, counts inline with data trailing, zero padding, little-endian? | **Accept**, on the understanding that the first service domain must report its round-trip cost before a second is built (N8) | §30 (validate all data crossing a boundary), §42 (no compiler layouts), §10 (booleans as checked bytes), P0, P1, rule 16 (security is not traded for performance), rule 20 (ABI compatibility separate from the release), rule 21 (performance must be measured), rule 35. No cost has been measured, so no performance claim is made. x86-64 only, per the directive's header. |
| Q2, answered | Handles in the body as a **slot holding the handle's index**, which must equal its position? | **Accept** | §30, §58 (a number in a message can never name a handle), ADR-0005 P1 and P8. It costs 4 bytes per handle and gives a canonical message (§7.4). |
| Q3, answered | A hello as ordinal 0 of every connection, with an interface id from SHA-256 and the negotiated minor `min(client, server)`? | **Accept** | N10, §30 (protocol versioning), §63, §59 (established algorithms only; SHA-256 here names an interface and is not a security property), S5, S9. The id does not prove identity: that is ADR-0005 B1. |
| Q4, answered | Exact size at the negotiated minor, refusing unknown trailing bytes, rather than accepting them? | **Exact** | N7, §13, P0. An ignored field is a security decision taken by accident. The cost is that a client must negotiate, which N10 already requires. |
| Q5, answered | A library whose version already exists in `origin-version.toml` names that key (`version compatibility "user-abi"`) instead of copying the number? | **Accept** | §62, §63, §79, P6: one place for each number. The alternative, a copied literal with a mechanical equality check, works but puts the number in two places. |
| Q6, answered | A named `capability` per library, with `requires` mandatory on every method, enforced by the stub through a predicate that does not cross the wire? | **Accept**, with the launcher's grant (N9) deciding the predicate for services | §30 (capability handles, access checks), §45 (declared capabilities), §58 (threat-model every privileged interface), rule 16. Making `requires` mandatory costs one word per method and makes an unauthorized method impossible to write by accident. |
| Q7, answered | A method's errors frozen for the whole major (a new failure mode gets a new method)? | **Accept** | N10 says only additions are allowed; a new error is an addition old clients cannot handle, so it is not one that compatibility can absorb (rule 39, §63). It can be relaxed later without breaking anyone; tightening later could not. |
| Q8, answered | `channel_read` reports only handle values, but N9's stub check needs each handle's object type and rights. Change `channel_read`'s handle array to records of `{u32 handle; u32 object_type; u32 rights}` in the slice that adds the stamped identity (B1)? | **Accept, and do it with B1**, while `user-abi` is at major 0 | §30, §42, N9, N14 step 3, migration note of ADR-0011: this is a change to what a read returns and it is cheapest before any third-party program exists (rule 39). Until that slice lands the stub's rights check runs only over a transport that supplies them, so it cannot be enforced on a real channel, and §7.4 says so. Recorded as ADR-0005 B3. |
| Q9, answered | A `string` is valid UTF-8 with no `NUL`, and is otherwise bytes for the service to interpret? | **Accept** | §51 (UTF-8), §42 (a C caller must not see a truncated string), §13. Normalization and path rules stay in each service. Interfaces that need bytes use `vector<u8, N>`. |
| Q10, answered | C symbols named `<library>_<method>`, for example `origin_kernel_channel_write`, with no shorter name for the kernel? | **Accept the mechanical rule** | §42 and §49 (stable, versioned C ABI), §7 (no special cases). A shorter `origin_channel_write` reads better, but it means a special case in the generator and a second convention to remember. It can still be changed before anything ships. |
| Q11, answered | Keep `event_create` and `object_signal` in the definition as `@reserved` until events exist, as `origin-abi` has them today? | **Keep them**, reserved | §42 (numbers are private, so they could go), §7, rule 35 ("no feature may be marked complete merely because it compiles"). Keeping them changes nothing and `@reserved` marks them as unimplemented in every generated output, including the generated dispatcher, which matches today's behaviour exactly (`NOT_SUPPORTED`). |
| Q12, answered | A type from another library is read with all its members known, because only the interface's own library is negotiated (§9)? | **Accept** | N10 (negotiate the version at connection time), §63, rule 39. The alternatives are a hello that negotiates every library a method touches, or forbidding `use` of a versioned type across libraries: the first has no first-party need yet and the second forbids sharing even `status` and `rights`. The closed enum still refuses a member the older side does not know. |
