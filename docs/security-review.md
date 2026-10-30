# HDJLabs security review

Scope: HDJLabs 1.3.0 — the Studio, the exported exam `.exe`, the LAN protocol and the code
runner. Review date: 2026-10-30. Tested on Windows 11 (build 26300), and across a VMware LAN with student
PCs on Windows 7 SP1 and Windows 8.1 (no GPU driver), using the debug and release builds.

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
* **Teacher re-check.** The Studio re-grades every submission from the student's answers with
  the full exam (four at a time, in the background). The score the student's PC reported is
  only shown until that finishes; a mismatch is flagged as *Score changed*. With *Keep the answer
  key off student PCs*, the exported exam carries no correct options, no accepted short answers
  (only their `***{**}` shape when the format hint is on) and no hidden tests.
* **Uploads.** File-upload questions refuse to export while lockdown is on, and the exam refuses
  to open a file window when its header says lockdown (the header is AEAD-bound, so it can't be
  edited). Files are capped at 3 MB per student, travel only inside the sealed submission, and
  are never part of live progress updates. Frames are read as bytes arrive, so a claimed length
  can't reserve memory by itself.
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
| 24 | Two student PCs (Windows 7, Windows 8.1) on a separate network, both in full lockdown | Live test over VMware NAT | Both found the teacher by broadcast, downloaded the exam (progress tracked), signed in, ran code, submitted; scores 3/20 and 1/20 arrived instantly |
| 25 | Student code on Windows 7 with no Node/Python installed | Live test | Built-in JavaScript ran inside the Job Object; tests passed 1/1 (hidden test passed at submission) |
| 26 | System state after lockdown on Windows 7 / 8.1 | Checked inside each VM | Policy keys back to their original (absent) state, no leftover backup file, sealed result backup present |
| 27 | Teacher removes the exam file from student PCs | Live test + unit test | Found a bug (see below); the fixed version is covered by a test deleting a file whose path has spaces and brackets |
| 28 | HTML preview used as an escape route (links, `window.open`, downloads, print, devtools, context menu, browser shortcuts) | Code review | Navigation limited to the page itself; new windows and downloads denied; print and context menu blocked; devtools and browser accelerator keys off |
| 29 | Exam exe run on a PC without a GPU driver | Live test (Windows 8.1, Basic Display Adapter) | OpenGL unavailable → relaunches with the CPU renderer automatically, inside lockdown |
| 30 | Missing runtime DLLs on old PCs | `dumpbin /dependents` | No VC++ runtime needed (static CRT); Windows-8-only DLLs are delay-loaded so the binary loads on Windows 7 |
| 31 | Forged score: a submission claiming full marks for wrong answers | Unit test | Teacher re-check scores it from the answers; the forged score is ignored and flagged |
| 32 | Answer key in the exported exam with *Keep the answer key off student PCs* | Unit test on the shipped JSON | No correct options, accepted answers or hidden tests present; only the answer shape |
| 33 | 40 students connecting, submitting and receiving grades at once | Load test on loopback | All 40 acknowledged and each received its own grade |
| 34 | Largest upload (3 MB) through the encrypted channel | Loopback test | Delivered byte for byte; live updates carry name and size only |
| 35 | File window in a locked exam | Code review + validation test | Export blocked while lockdown is on; the exam itself refuses uploads when its header says lockdown |
| 36 | Antivirus | Microsoft Defender scan (signatures of 2026-10-30) of the release Studio and an exported exam | No threats found in either file. Other antivirus products can still differ; see *Prepare the lab PCs* in the classroom guide |
| 37 | Keep-answers-off exam, live on Windows 7 SP1 and Windows 8.1 (no GPU driver) in lockdown | VMware LAN, release build | Each student PC scored itself 0 (it has no answer key); the teacher re-graded (Windows 7: 1/22 for the one right answer) and both screens showed "Graded by your teacher" with the teacher's score |
| 38 | HTML preview on a PC without a graphics driver | Live test (Windows 8.1) | Found a bug (see below); after the fix the question shows a notice and every dialog is clickable |
| 39 | Exam exported from a code-signed Studio | `signtool` + `Get-AuthenticodeSignature` with a test certificate | Exam carried in the signature's certificate bag: signature intact. Old appended format: `NotSigned`. One byte of code changed: `HashMismatch`. The signed exam opens and runs |
| 40 | Code question with no expected output (left in "compare output" mode) | Unit test + capture | Treated as teacher-graded: the student waits for the teacher's score instead of seeing 0 for it |
| 41 | SmartScreen and the intranet zone | Live test (Windows 8.1) | The same exam shows SmartScreen when marked as an Internet download and opens directly when marked Intranet; `lab-trust-teacher.ps1` moves the teacher's address to Intranet without admin rights |
| 42 | Arabic and mixed text editing | Scripted keyboard input + captures | Joined letters, lam-alef ligature, right-aligned RTL lines, correct order for English inside Arabic, caret, arrows, selection, wrapping and Enter in multi-line fields |

## Findings fixed during the review

| Severity | Finding | Fix |
|---|---|---|
| High | Student code could start new processes, and from the private desktop that includes `explorer.exe`, a full shell. | Run step uses `ActiveProcessLimit = 1`, starts suspended and is assigned to the job before it runs; no console host. |
| Medium | Unbounded thread-per-connection on the teacher's ports allowed a LAN flood. | Concurrent caps (400 student connections, 48 HTTP) plus a 1,000-attempt roster limit. |
| Medium | Closing the exam mid-attempt and resuming was invisible to the teacher. | Resume is recorded as a violation and sent live. |
| Medium | Sticky Keys and similar accessibility hotkeys open system dialogs (a classic kiosk escape). | Hotkeys disabled for the duration and restored afterwards. |
| Low | Live answer snapshots failed to decode (integer map keys inside a tagged enum), forcing a reconnect. | Sent as a list of pairs; regression test added. |
| Low | Port probes (connect, then close) were logged as errors. | Treated as a normal disconnect. |
| High | Exams didn't start at all on Windows 7 (missing `GetSystemTimePreciseAsFileTime`) or on PCs without a GPU driver. | Universal Windows 7 target build; CPU renderer fallback. |
| Medium | Before sign-in, student PCs reconnected to the teacher every 2 s and showed "Offline". | Clients wait at "Teacher found" until sign-in; clear connection stages. |
| Medium | "Remove exam" did not delete the file: the command line reached `cmd.exe` with Rust-style `\"` escaping, and the CPU renderer skipped the exit callback. | Raw command line; cleanup runs explicitly before closing. Regression test added. |
| Low | The HTML preview rendered pages dark (WebView2 auto-dark) unlike a real browser. | Light theme and a zero-specificity white default. |
| High | On a PC without a graphics driver (Windows 8.1, CPU renderer) the WebView2 window ignored "hide" and stayed on screen invisible, covering the submit dialog and swallowing its clicks. | No live preview under the CPU renderer (a notice explains it); a hidden preview is also shrunk to zero size. Retested live. |
| Medium | The exam's own score was trusted by the teacher. | Every submission is re-graded on the teacher's PC; mismatches are flagged. Optional answer-free export. |

## Known limitations

* **Ctrl+Alt+Del cannot be blocked** by any application; it's the Secure Attention Sequence.
  HDJLabs removes everything useful from that screen. *Switch user* can only be hidden by machine
  policy (`HideFastUserSwitching`, needs admin); switching users doesn't close or reveal the exam.
* **Answers are in the exam file by default**, encrypted under the exam password, so the exam can
  grade instantly offline. Tick *Keep the answer key off student PCs* for high-stakes exams: the
  Studio then grades and sends scores over the LAN (students without a connection get theirs
  later).
* **Result files can be forged** by someone who extracts the teacher's public key and writes
  their own submission. The score inside is re-graded on the teacher's PC, so only the answers
  matter; the Live classroom also saw the answers as they were written. Prefer live collection.
* **Student code has network access.** Firewalling it would need admin rights. Use a
  classroom network without internet, which is the intended setup anyway.
* **Hardware, VMs and other devices** (phones, smart glasses, a second PC) are out of scope. VM
  detection is a best-effort BIOS-string check.
* **Code signing** is wired into the release workflow (SignPath Foundation, free for open
  source) and switches on once the project is accepted; until then releases are unsigned. Even
  signed, a new file can show SmartScreen until reputation builds up; in the lab the intranet
  zone setting avoids it (see `docs/code-signing.md`).

## Reporting

Please report vulnerabilities privately through GitHub Security Advisories on
[op-h/HDJLabs](https://github.com/op-h/HDJLabs/security/advisories/new).
