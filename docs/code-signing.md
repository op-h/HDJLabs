# Code signing and SmartScreen

Windows shows **"Windows protected your PC"** (SmartScreen) the first time it sees an unsigned
program downloaded from the internet. This page covers the two ways HDJLabs deals with that:
signing every release (free for open-source projects), and, for the lab, a setting that stops
SmartScreen from checking exams downloaded from the teacher's PC at all.

## How exams stay signed

An exported exam is the Studio program with the encrypted exam attached. Appending data after
an Authenticode signature makes Windows treat the file as **unsigned** (tested: `NotSigned`).
HDJLabs therefore carries the exam inside the signature's certificate bag when the Studio is
signed, the same technique Chrome uses to tag its signed installers (`src/sigtag.rs`). The
signed bytes are untouched, so every exam a signed Studio exports is signed by the same
publisher. Changing any byte of the program still breaks the signature (`HashMismatch`).

Tested with a real signature (`signtool`, self-signed test certificate):

| File | Windows says |
|---|---|
| Signed Studio | Signed (test publisher) |
| Exam exported by the signed Studio | **Signed, signature intact** |
| Exam built the old way (bytes appended) | NotSigned |
| Exam with one byte of code changed | HashMismatch |

## Free signing: SignPath Foundation

[SignPath Foundation](https://signpath.org/) signs open-source projects for free with its own
certificate, so the publisher Windows shows is SignPath Foundation. HDJLabs qualifies: MIT license, public repository,
releases built by GitHub Actions from source.

1. **Apply** at signpath.org (Apply / Get started). You'll need: the repository URL
   (github.com/op-h/HDJLabs), the license (MIT), and a short description. Their conditions
   include: no proprietary components, signing only builds made by CI from the public source,
   the same people maintain and sign, and **multi-factor authentication** on GitHub and SignPath
   for everyone on the project. Add a "Code signing policy" section to the README when asked
   (they provide the wording).
2. Once accepted, in the SignPath web app:
   * create the project `hdjlabs` linked to the repository;
   * add two **artifact configurations** with the slugs `hdjlabs-exe` and `hdjlabs-setup`,
     pasting the XML from [`.signpath/hdjlabs-exe.xml`](../.signpath/hdjlabs-exe.xml) and
     [`.signpath/hdjlabs-setup.xml`](../.signpath/hdjlabs-setup.xml);
   * use the **release-signing** policy they set up (note its slug);
   * create a **CI user** and copy its API token.
3. In GitHub → repository **Settings → Secrets and variables → Actions**:
   * secret `SIGNPATH_API_TOKEN` = the CI user's token;
   * variables `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG` (`hdjlabs`) and
     `SIGNPATH_POLICY_SLUG` (e.g. `release-signing`).
4. Push a new version tag. The release workflow now uploads `HDJLabs.exe`, waits for SignPath to
   sign it, builds the installer around the signed program, signs the installer, and refuses to
   publish if either signature isn't `Valid`. Until the variable exists, these steps are skipped
   and releases stay unsigned as before.

### If SignPath says no

| Option | Cost | Notes |
|---|---|---|
| Azure Artifact Signing | about $9.99 / month | Individuals only in the USA and Canada; organisations in more countries. |
| OV code-signing certificate (Certum, Sectigo resellers…) | roughly $100–400 / year | Needs a hardware token or cloud HSM. Replace the SignPath steps with `signtool sign`. |
| Certum "Open Source" certificate | lower, individual | Reports differ on whether Windows trusts it for SmartScreen; check before buying. |

## What signing does and doesn't do

* Signing shows a real publisher instead of "Unknown publisher" and proves the file wasn't
  altered. Since 2024 even EV certificates no longer skip SmartScreen: reputation builds up
  for the certificate and the file as people run it. Expect warnings on the first downloads of
  a new release to fade as reputation accumulates.
* Every exported exam is a new file, so SmartScreen can still show a warning for a brand-new
  exam until the publisher's reputation is established. For the lab, use the setting below.

## In the lab: no SmartScreen at all, free

SmartScreen only checks files downloaded from the **Internet** zone. Put the teacher's PC in
the **Local intranet** zone and exams downloaded from it open straight away. Tested on
Windows 8.1: the same exam showed "Windows protected your PC" when marked as an Internet
download and opened directly when marked as Intranet, and the mapping below changed Windows'
classification of the teacher's address from Internet to Intranet.

* **One PC, no admin:** on each student PC press Win+R and paste (the script is installed
  with the Studio; copy it from the teacher's PC to a USB stick if the Studio isn't installed there):
  ```
  powershell -ExecutionPolicy Bypass -File "%LOCALAPPDATA%\Programs\HDJLabs\tools\lab-trust-teacher.ps1" -Teacher 192.168.1.20
  ```
  Use the address on your Live classroom page, or a pattern such as `192.168.1.*`.
  `-Remove` undoes it.
* **Whole lab:** the admin sets Group Policy *Internet Explorer → Internet Control Panel →
  Security Page → Site to Zone Assignment List* with `http://192.168.1.20` = `1`.
* Copying the exam with a USB stick also avoids SmartScreen: files copied from a USB stick
  aren't marked as internet downloads.
