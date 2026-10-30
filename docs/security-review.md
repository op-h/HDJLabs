# HDJLabs security review

Scope: HDJLabs 1.0.0 — the Studio, the exported exam `.exe`, the LAN protocol and the code
runner. Review date: 2026-10-30. Tested on Windows 11 (build 26300) with the debug and release
builds.

## Threat model

| Actor | Goal | In scope |
|---|---|---|
| Student before the exam | Read the questions early, find the answers in the file | Yes |
| Student during the exam | Leave the exam, look things up, use other apps, copy in text, screenshot or stream the screen, get remote help | Yes |
| Student with code questions | Use their program to escape the sandbox or reach the desktop | Yes |
| Student on the LAN | Read or forge other students' traffic, impersonate the teacher, crash the teacher's Studio | Yes |
| Student after the exam | Change their score, forge a result file | Yes |
| Someone with admin rights / a kernel debugger / a second device / a phone camera | Anything | **No.** No user-mode exam app can stop these; physical proctoring still matters. |

## Design

* **Exam at rest.** The exam JSON is encrypted with ChaCha20-Poly1305 under an Argon2id key
  derived from the exam password (19 MiB, 2 passes, about 0.3 s per guess). The readable header
  (title, duration, lockdown flags) is bound in as AAD, so editing it makes the exam undecryptable.
  The teacher's reference solutions and both passwords are stripped before export. The proctor
  password ships only as an Argon2id hash.
* **Results.** Each submission is sealed to the teacher's X25519 public key: ephemeral ECDH →
  HKDF-SHA256 → ChaCha20-Poly1305. Only the Studio project that exported the exam can open it,
  and any modification is detected.
* **LAN.** The client opens with an ephemeral X25519 key. Every frame afterwards is AEAD-encrypted
  under a key only the real teacher can derive, with per-direction nonce counters, so replayed,
  reordered or forged frames fail.
* **Lockdown.** A private Win32 desktop, a low-level keyboard hook, per-user policies, a
  capture-excluded window and pre-flight checks. All system changes are backed up first and
  restored on exit or on the next launch.
* **Code sandbox.** Every run executes in a Job Object: `ActiveProcessLimit = 1` for student code,
  1 GiB memory, UI restrictions (desktop, handles, clipboard, global atoms, system parameters,
  exit Windows), kill-on-close, a 6 s timeout and a 128 KiB output cap. Processes start suspended
  and are assigned to the job before their first instruction runs.

## Tests performed

Automated tests live in the source tree and run in CI (`cargo test`, 19 tests).

| # | Attack / check | Method | Result |
|---|---|---|---|
| 1 | Open exam with wrong password | `payload::tests::roundtrip_and_wrong_password` | Rejected |
| 2 | Flip `lockdown:false` in the exe's header | `payload::tests::header_tamper_breaks_exam` | Exam no longer decrypts |
| 3 | Find the teacher's reference solution in the exe | Byte search of the exported file | Not present |
| 4 | Forge or read another exam's result file | `crypto::tests::sealed_box_roundtrip_and_tamper` | Wrong key and changed exam id both rejected |
| 5 | Replay / reorder LAN frames | `crypto::tests::session_lockstep` | Rejected |
| 6 | Impersonate the teacher with a different key | Same test | Client cannot decrypt, so it refuses |
| 7 | Garbage, wrong exam id, 4 GB frame length on the exam port | `net::tests::rejects_wrong_exam_and_garbage` + live probe | Rejected, server unaffected, nothing added to the roster |
| 8 | Slow-loris partial handshake | Live probe (`HDJ1` then silence) | 10 s handshake timeout; other clients unaffected |
| 9 | UDP discovery for a different exam | Live probe | No reply |
| 10 | Path traversal on the download page (`/../`, `%2f`, `C:/`, `//etc/passwd`, `%00`) | Live probe | 404 for all; only `/` and `/download` exist |
| 11 | HTML injection through the exam title on the download page | Live probe | Escaped; strict `Content-Security-Policy` |
| 12 | 100 KB request line | Live probe | Read capped at 4 KB, rejected |
| 13 | Connection flood (120 sockets) on the download page | Live probe | Capped at 48 concurrent (503 beyond that); page served normally afterwards |
| 14 | Student code launches a process (`subprocess.run(['cmd', ...])`) | `runner::tests::sandbox_blocks_child_processes` | Blocked by the job's process limit |
| 15 | Infinite loop | `runner::tests::infinite_loop_is_killed` | Killed at the time limit |
| 16 | 10 MB of output | `runner::tests::output_is_capped` | Truncated at 128 KiB |
| 17 | `os.system` in the built-in Python | Manual | Not available |
| 18 | Private desktop actually used | Live run with the self-test build | Exam rendered on `HDJLabsExam-…` in Locked mode, window hardened |
| 19 | Policies applied during the exam and restored after | Registry check during and after a live run | Task Manager, Lock, Sign out, Power, 3/4-finger gestures and edge swipe off during; every value restored exactly after; backup file removed |
| 20 | Run the exam by hand with `--exam-child` to skip the launcher | Code path | Not on the private desktop, so it falls back to in-window lockdown with its own policy guard |
| 21 | Kill or crash the exam, relaunch, continue | Code path | Attempt resumes (time keeps running); "Exam was closed and resumed" is logged to the teacher |
| 22 | Brute-force the exam password in the UI | Code path | Exponential cooldown after 3 failures (up to 60 s); each failure reported to the teacher |
| 23 | Debug self-test hooks (auto-close, screenshots) usable in production | Build config | Compiled out of release builds (`cfg(debug_assertions)`) |

## Findings fixed during the review

| Severity | Finding | Fix |
|---|---|---|
| High | Student code could start new processes, and from the private desktop that includes `explorer.exe`, a full shell. | Run step uses `ActiveProcessLimit = 1`, starts suspended and is assigned to the job before it runs; no console host. |
| Medium | Unbounded thread-per-connection on the teacher's ports allowed a LAN flood. | Concurrent caps (400 student connections, 48 HTTP) plus a 1,000-attempt roster limit. |
| Medium | Closing the exam mid-attempt and resuming was invisible to the teacher. | Resume is recorded as a violation and sent live. |
| Medium | Sticky Keys and similar accessibility hotkeys open system dialogs (a classic kiosk escape). | Hotkeys disabled for the duration and restored afterwards. |
| Low | Live answer snapshots failed to decode (integer map keys inside a tagged enum), forcing a reconnect. | Sent as a list of pairs; regression test added. |
| Low | Port probes (connect, then close) were logged as errors. | Treated as a normal disconnect. |

## Known limitations

* **Ctrl+Alt+Del cannot be blocked** by any application; it's the Secure Attention Sequence.
  HDJLabs removes everything useful from that screen. *Switch user* can only be hidden by machine
  policy (`HideFastUserSwitching`, needs admin); switching users doesn't close or reveal the exam.
* **Answers are in the exam file**, encrypted under the exam password, so the exam can grade
  instantly offline. A student who knows the password and reverse-engineers the exe *during*
  the exam could extract them, but lockdown leaves no tools to do it with. For high-stakes use,
  rely on the LAN session and treat on-device scores as provisional.
* **Result files can be forged** by someone who extracts the teacher's public key and writes
  their own submission. The Live classroom sees the real-time progress and answers, so a forged
  file wouldn't match what the teacher watched. Prefer live collection.
* **Student code has network access.** Firewalling it would need admin rights. Use a
  classroom network without internet, which is the intended setup anyway.
* **Hardware, VMs and other devices** (phones, smart glasses, a second PC) are out of scope. VM
  detection is a best-effort BIOS-string check.
* **Not code-signed yet**, so SmartScreen warns on first run.

## Reporting

Please report vulnerabilities privately through GitHub Security Advisories on
[op-h/HDJLabs](https://github.com/op-h/HDJLabs/security/advisories/new).
