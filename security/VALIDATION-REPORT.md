# Validation report: SECURITY-REVIEW findings, executed

Companion to [`EXPLOIT-MAPPING.md`](EXPLOIT-MAPPING.md). This is the result of
turning each finding in [`refs/hero-siege/SECURITY-REVIEW.md`](../refs/hero-siege/SECURITY-REVIEW.md)
into a runnable probe and executing it under the sandbox described in the mapping
document.

**Result: 12 of 12 probes confirmed, 128 individual predictions, 0 failed.** One
finding (F-10) has its structural claim confirmed and its stated mechanism
refuted; three places where the artefacts disagree with the review are recorded
in §3.

Everything below is local, read-only, offline work on a licensed copy. No game
process was patched, no DLL replaced, no packet forged, no service contacted, no
credential used. See §2 of the mapping document for the boundary.

## Environment

| | |
| --- | --- |
| kernel | Linux 7.2.3-1-cachyos x86_64 |
| sandbox | bubblewrap 0.12.0 (user/ipc/pid/net/uts/cgroup namespaces, seccomp, prlimit) |
| seccomp policy | 104 cBPF instructions, 48 entries (denylist, `EPERM`) |
| python | 3.14.7 (stdlib only) |
| binutils | GNU objdump 2.47 (`nm`, `objdump`) |
| capture | `refs/hero-siege`, commit `fd39ee2`, all 6 `PROVENANCE.sha256` inputs verified OK |

Reproduce:

```bash
cd refs/hero-siege/lab && ./run-all.sh      # ~4 s; writes results/
```

## 1. Summary

| id | sev | probe | class | verdict | predictions | evidence |
| --- | --- | --- | --- | --- | --- | --- |
| F-00 | — | `f00_sandbox_selftest` | — | confirmed | 12/12 | dynamic |
| F-01 | High | `f01_linux_extension_subset` | S0 | confirmed | 11/11 | static |
| F-02 | High | `f02_pool_plaintext` | S2 | confirmed | 10/10 | dynamic |
| F-02b | High | `f02b_windows_pool_layout` | S1 | confirmed | 14/14 | static + model |
| F-03 | High | `f03_unseeded_rand` | S1 | confirmed | 9/9 | static + model |
| F-04 | Medium | `f04_protection_assertion` | S0 + S4 | confirmed | 13/13 | static + analogue |
| F-05 | Medium | `f05_keyless_hashes` | S0 | confirmed | 11/11 | static |
| F-06 | Low | `f06_freevariable_oob` | S0 + S2 | confirmed | 9/9 | static + dynamic |
| F-07 | Low | `f07_tool_hardening` | S3 | confirmed | 13/13 | static + dynamic |
| F-08 | Info | `f08_provenance` | S0 | confirmed | 9/9 | static |
| F-09 | Info | `f09_embedded_keys` | S0 | confirmed | 9/9 | static |
| F-10 | Info | `f10_undeclared_exports` | S0 | structural confirmed / **mechanism refuted** | 8/8 | static |

The severity ordering is the review's, not a re-assessment.

## 2. Per finding

### F-01 — the Linux extension is a strict subset (High) — confirmed

`extracted/datawin/extensions.txt` declares eight `ac_dll_gm` functions to
GameMaker. The two artefacts are not equivalent:

| | declared | exported |
| --- | --- | --- |
| `ac_dll_gm.so` | 8 | 6 |
| `ac_dll_gm.dll` | 8 | 10 |

`TamperCheck` and `MemoryTamperCheck` are declared and called, and absent from
the Linux build. The `.so` export set is a strict subset of the `.dll`'s; the
four DLL-only names are `TamperCheck`, `MemoryTamperCheck`, `ProtectVariable`,
`ScrambleKey`. `gml_Script_PC_TamperCheckGMLWrapper` exists in the decompiled
set, so the missing export is not hypothetical.

Linux also ships no EAC enforcement path: `linux/run.sh` is three lines and never
invokes `start_protected_game`, and there is no `linux/EasyAntiCheat/` directory.

### F-02 — the pool does not protect values (High) — confirmed

**Linux (dynamic).** `InitNewVariableFast(987654.321)` returns slot id 0.
Walking `memoryPage[id >> 9]->varPool[id & 0x1ff]` from the resolved
`memoryPage` symbol (`0x4080` in the `.so`) yields:

```
slot 0x5556…0e10: value=987654.321 inUse=True isUndefined=False
slot bytes: ac 1c 5a a4 0c 24 2e 41 01 00 …
expected  : ac 1c 5a a4 0c 24 2e 41        (struct.pack('<d', 987654.321))
```

The stored bytes are the raw IEEE-754 encoding. A blind scan of anonymous memory
finds the same 8 bytes in 9 places, including the slot address. The pool is also
enumerable from outside: unused slots read `-1.0` and undefined slots read the
`GM_UNDEFINED` sentinel `-696969696969.42`.

**Windows (model).** The mixing function was transcribed from the disassembly at
`TamperCheck` (0x1800019cb) — GOLDEN `0x9e3779b97f4a7c15`, `ror 51`, `shr 7`,
multiplier `0xbf58476d1ce4e5b9`, `shr 17` — and then applied to 2000 random
`(key, value)` pairs:

- `check ^ mix(key)` reproduced the plaintext in **2000/2000** trials, without
  ever following the `+0x0` pointer to the ciphertext cell;
- the two slot fields were mutually consistent in **2000/2000** trials.

`key` is stored in the clear at `slot+0x18` (`mov QWORD PTR [rbx+0x18],rcx`).
The claim that the anti-scan design is self-defeating holds: the pool decrypts
from two adjacent qwords.

### F-03 — unseeded `rand()` is the scramble key (High) — confirmed

Parsed from the PE import directory rather than a text search:

- `rand` is imported from `api-ms-win-crt-utility-l1-1-0.dll`;
- **`srand` is not imported anywhere in the image**;
- the call target at IAT `0x180003118` resolves to `(api-ms-win-crt-utility-l1-1-0.dll, rand)`;
- `InitNewVariableFast` calls that slot and stores the sign-extended return
  straight into `slot+0x18`.

The resulting stream is a build-time constant. MSVC `rand()` with the default
seed 1 gives

```
[41, 18467, 6334, 26500, 19169, 15724, 11478, 29358, …]
```

Two independent "machines" derive the identical stream, every value is 15-bit,
and the pool holds 262144 slots against at most 32768 distinct keys — so keys are
forced to repeat, and the "scramble" is not even a permutation of the slot space.
The key for the first allocated slot is the constant 41 on every install.

### F-04 — `MemoryTamperCheck` is a page-permission assertion (Medium) — confirmed

**Static.** The function calls `VirtualQuery` (IAT `0x180003010`), reads
`mbi.Protect`, and accepts only `PAGE_READONLY` (0x2) or `PAGE_EXECUTE_READ`
(0x20). No byte-content comparison appears anywhere in the function.
`ProtectVariable` re-applies `PAGE_READONLY` and pops a `MessageBoxA` on failure.

**Analogue.** A private anonymous page was filled, `mprotect`ed to `PROT_READ`,
and then:

| action | result |
| --- | --- |
| direct store in a forked child | dies with `SIGSEGV` — the protection is real |
| write via `/proc/self/mem` | succeeds, 5 bytes written |
| `/proc/self/maps` before / after | `r--p` / `r--p` — **unchanged** |
| read back | `PWNED` |

A checker that asks only "is the page still read-only?" reports clean, because
there is no observation available to it that distinguishes this state from an
untouched page. The same argument covers `ptrace`, a kernel driver, DMA and VM
introspection. The child's `SIGSEGV` left no core dump — `RLIMIT_CORE=0` is
asserted in the same probe.

### F-05 — the integrity hashes carry no secret (Medium) — confirmed

Over all 35 decompiled GML scripts:

- `string_sha256` is called at **8 sites**, each with exactly **2 GML arguments** —
  there is no third key or nonce slot;
- only four scripts hash anything: `AntiCheatGenerateHashes`, `CheckRoomHash`,
  `GetCounterHash`, `GetGoldCounterHash`;
- **zero** occurrences of `hmac`, `nonce`, `secret`, `shared_key`, `server_key`,
  `signing_key`, `private_key`, `mac_key`;
- `GPVSAFE`'s literal `0xc26448d432d92d71` is exactly `GM_UNDEFINED`
  (`-696969696969.42`); on an undefined read it writes the sentinel and calls two
  **client-side** functions, `gml_Script_ReportClient` and `gml_Script_LogOut`;
- `AntiCheatGenerateHashes` pairs `DecryptStringApi` (×5) with `string_sha256`
  (×5) — a name → id index, not an authenticator.

The digest is a pure function of two client-known inputs and is recomputable by
anyone holding the build. The probe establishes that no secret exists; it does
not construct a packet or contact a server.

### F-06 — check after use in `FreeVariable` (Low) — confirmed

**Static.** The load precedes the check:

```
14ce  mov rax,[rcx+rax*8]   <- pool page load
14d2  cmp edx,0x3ffff
14d8  ja  1508              <- bounds check after the load
```

**Dynamic, address-pinned.** `cvttsd2si` of an out-of-range double yields the
integer-indefinite pattern `0x80000000`; `cdqe` sign-extends it; the load becomes
`*(memoryPage + (int64)(int32)(idf/512) * 8)`, i.e. **`memoryPage − 16 GiB`**.

Every out-of-range input faults:

```
1e300     SIGSEGV      -1e300    SIGSEGV      2^62   SIGSEGV
nan       SIGSEGV      inf       SIGSEGV      -inf   SIGSEGV
```

A valid slot id does not crash and returns `-1.0`. Then the decisive pair:

| child | mapping at the predicted address | result |
| --- | --- | --- |
| A | absent | `SIGSEGV` |
| B | present (`MAP_FIXED_NOREPLACE`) | returns `-1.0`, no fault |

The only difference is the mapping, so the fault address is pinned rather than
merely observed. Impact remains a self-inflicted crash, as the review says — but
the address is now known exactly, which is what makes the fix testable.

### F-07 — the analysis tools are unbounded on untrusted input (Low) — confirmed

| input | observed |
| --- | --- |
| 59-byte `2zoq` container declaring 65535×65535 | `MemoryError` allocating 17,179,344,900 bytes |
| 208-byte bz2 stream | `MemoryError` decompressing to 256 MB (**1,290,555×**) |
| 5-byte truncated header | `struct.error` escapes `main()`'s `except YytexError` |
| missing file | `FileNotFoundError` escapes the same handler |
| ELF with `e_shoff` past EOF | `struct.error` from `elf.py` (no bounds check) |
| ELF with a bogus `shstr_size` | `MemoryError` — an unbounded read driven by a header field |
| `ehframe.Reader.cstr()` with no NUL | silently rewinds `pos` to 0 |
| `ehframe.Reader.u8()` past the end | `IndexError` |

Each dynamic case runs in a child with its own tightened `RLIMIT_AS`, so
containment is demonstrated by the probe rather than inherited from the harness.
Consistent with the review: these are crashes and memory exhaustion, not memory
corruption — Python slicing and `struct` are bounds-safe.

One case is a *second* `elf.py` path the review did not list: with `e_shoff = 0`,
the ELF header is parsed as a section header and the resulting bogus `shstr_size`
drives the unbounded read. Both paths are now covered.

### F-08 — information exposure and provenance gaps (Info) — confirmed

The manifest is **accurate**: all six listed files hash as recorded, including
both anti-tamper extensions. It is also **narrow**:

```
6 covered of 120,636 files in the capture (0.005%)
```

Not covered: `PAS_Steam_GMS_x64.dll`, `EOSSDK-Win64-Shipping.dll`,
`HS_Windows_Extension.dll`, `start_protected_game.exe`,
`PAS_Steam_GMS_x64.so`, `libEOSSDK-Linux-Shipping.so`, `Steamworks.so`, and all
**29 files** under `bin/EasyAntiCheat/`. There is no companion signature, so the
manifest cannot attest to its own integrity either.

Build-environment exposure confirmed: the `.so` retains DWARF with
`DW_AT_comp_dir = /home/jussi/Desktop/paskac/ext/ac_dll_gm/BuildOther` and source
`../ac_dll_gm/ac_linux.cpp`, and `bin/options.ini` names the Visual Studio 2019
install path.

### F-09 — save and API encryption keys are embedded constants (Info) — confirmed

The probe did not take the quoted tables on trust. It extracted every 64-bit
immediate from `gml_GlobalScript__EncryptionFuncs` and decoded each as a double:

- exactly **32** distinct immediates, and they decode to **exactly** the two
  quoted 16-entry tables — all 32 values present, no extras;
- all **16** crypto entry points (`Encrypt`/`Decrypt` × `String`, `Number`,
  `Mixed`, `Array`, `StringSave`, `StringApi`, plus `PilipaliEncrypt`,
  `PilipaliDecrypt`, `PaskaCrypt`, `Rypteri`) are registered from this one script;
- `EncryptNumber` delegates to `EncryptString` and carries no constants of its
  own, so there is one scheme rather than two;
- save data and API payloads share the same primitives.

Round-trippable by anyone holding the build, as the review concludes.

### F-10 — two exports are undeclared in GameMaker (Info) — **structural claim confirmed, mechanism refuted**

Confirmed: the DLL exports 10 names, GameMaker is told about 8, and the two
extras are exactly `ProtectVariable` and `ScrambleKey`.

Refuted: the review's operational conclusion. F-10 states the two extras are
"called from the allocator/setter paths described in F-02 to F-04", and that a
DLL-replacement stubbing only the eight declared functions "leaves live call
sites to the other two, so a naive swap crashes rather than silently no-ops".

Across the whole image:

- **no** direct `call` to either RVA — the only textual occurrence of each
  address is its own function prologue (43 distinct direct call targets exist in
  `.text`, so the scan works);
- **no** absolute 8-byte pointer to either RVA anywhere in the file, so there is
  no indirect call site either;
- each RVA occurs exactly twice: its own `.pdata` unwind entry and its slot in
  the export table;
- `InitNewVariableFast` **inlines** its own `rand` + `VirtualProtect` sequence via
  direct IAT calls instead of calling `ScrambleKey`/`ProtectVariable`.

So the two extras are exported dead code with respect to the DLL's own logic. A
stub DLL that omits them does not crash; it silently drops two functions that
were never invoked. The structural half of F-10 stands — the exported names are
still not a reliable inventory of the code — but the failure mode it predicts is
wrong, and the remediation note that depends on it ("a naive swap crashes") should
be dropped.

## 3. Where the artefacts and the review disagree

1. **F-02's "splitmix64" label is approximate.** The compiled mixing function is
   splitmix64-*shaped* — canonical GOLDEN increment and `0xbf58476d1ce4e5b9`
   multiplier — but uses `ror 51` / `shr 7` / `shr 17`, not splitmix64's
   `shr 30` / `shr 27` / `shr 31`. Worked example: for input
   `0x1234567890ABCDEF` the compiled function gives `0x9d54ea73001ff672` and
   canonical splitmix64 gives `0xd7d7fb52e5aefe5c`. The finding is unaffected, but
   a model that used canonical splitmix64 would not reproduce the pool's algebra.
2. **The capture contradicts itself about F-06.** The machine code loads before
   the bounds check; the vendored Ghidra output
   (`extracted/native/ac_decompiled/FreeVariable.c`) shows the check *before* the
   dereference, because the decompiler hoisted the load into the `&&`
   short-circuit. Anyone reading only the `.c` would conclude F-06 is a
   false positive. The `.c` should carry a note, or the finding should cite the
   disassembly.
3. **F-10's mechanism claim is false** (§2, F-10).

## 4. Observations the review did not make

- **`FreeVariable`'s fault address is a constant.** Every out-of-range magnitude
  and NaN collapse to the same `0x80000000` index, so the wild read is always
  `memoryPage − 16 GiB` — not a range. That makes the fix (`cmp` before `cdqe`)
  directly testable, and makes a regression test cheap.
- **The pool's key entropy is 15 bits, not 32.** `rand()` returns `0..32767`, so
  the 262144-slot pool cannot be covered by distinct keys even if the LCG were
  seeded per-install.
- **`elf.py` has a second unbounded path** beyond the one the review lists: a
  bogus `shstr_size` (reachable with `e_shoff = 0`) drives
  `self.f.read(shstr_size)` to `MemoryError`.
- **Provenance coverage is 0.005%**, not "6 of ~90": the capture contains 120,636
  files. The review's "~90" appears to count top-level entries rather than files.
- **The seccomp filter needed a self-test.** The first generator produced a filter
  that loaded successfully and denied exactly one of forty-eight syscalls; see
  §5 of the mapping document. Reported here because a validation lab that cannot
  detect its own degradation is worse than no lab.

## 5. Remediation items now backed by a reproducer

| review §5 item | reproducer |
| --- | --- |
| 1. move integrity off the client | F-05 — no key exists, and every response is client-side |
| 2. key the hashes or drop them | F-05 — 2 arguments, no key material anywhere |
| 3. fix F-06 at the source | F-06 — exact fault address, address-pinned pair of children |
| 4. replace `rand()` / drop the contrived key | F-03 (key stream), F-02b (the `check` field leaks plaintext) |
| 5. decide the Linux posture deliberately | F-01 — subset exports, no EAC path, `run.sh` bypasses all of it |
| 6. harden the tooling | F-07 — four contained failures, two unbounded-allocation paths |
| 7. close the provenance gap | F-08 — 6/120,636, unsigned, EAC tree uncovered |
| 8. strip DWARF / normalise build paths | F-08 — `DW_AT_comp_dir` with the author's home directory |

Items 3, 6 and 7 are now cheap to regression-test: the probes are deterministic
and run in about 4 seconds in total.

## 6. Not testable in this pass

- **EAC's enforcement component.** Not vendored; downloaded at install time. F-01
  can only confirm its absence from the Linux depot, which it does.
- **The PS5 and Android tamper-check backends.** No runner vendored, as the review
  also notes.
- **Windows runtime behaviour.** No Windows host. F-02b, F-03, F-04 and F-10 are
  static plus model; the model probes assert their own transcription against the
  disassembly to keep that honest.
- **Any server-side consequence.** Out of scope by the boundary in §2 of the
  mapping document. F-05 deliberately stops at "no secret exists".
