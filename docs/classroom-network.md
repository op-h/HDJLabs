# Running HDJLabs on a classroom network

HDJLabs needs a local network, not the internet. The teacher's PC and the students' PCs just
have to reach each other.

## Pick a network

| Setup | How |
|---|---|
| School lab LAN | Use it as is. Unplugging the internet uplink is fine. |
| Any Wi-Fi router or switch | Plug or connect everyone to it. It doesn't need an internet connection. |
| No router at all | On the teacher's laptop, open **Settings → Network & internet → Mobile hotspot**, turn it on, and have students join that Wi-Fi. Up to 8 devices on most adapters. |

Avoid networks with **client isolation** ("AP isolation", common on guest Wi-Fi). It stops
devices from seeing each other.

## Prepare the lab PCs

Do this once, a day before the exam, on one lab PC:

1. **Run a practice exam** (*Preview* or any exported exam with lockdown) and submit it.
   **SmartScreen:** if Windows says *"Windows protected your PC"*, either choose *More info → Run
   anyway*, or stop it for good by putting your PC in the intranet zone on each student PC (no
   admin needed; press Win+R and paste):
   ```
   powershell -ExecutionPolicy Bypass -File "%LOCALAPPDATA%\Programs\HDJLabs\tools\lab-trust-teacher.ps1" -Teacher 192.168.1.20
   ```
   (use the address on your Live classroom page; details and the Group Policy for a whole lab
   in [code signing and SmartScreen](code-signing.md)).
2. **Antivirus.** An exam blocks shortcuts with a keyboard hook, deletes itself when asked and
   carries an encrypted payload, which some antivirus products flag. HDJLabs 1.2.0 passes a
   Microsoft Defender scan; if the lab uses another product and it blocks or quarantines the
   exam, ask the lab admin to allow the folder students run exams from, for example:

   ```powershell
   # Microsoft Defender, run as administrator. Replace the folder with yours.
   Add-MpPreference -ExclusionPath "C:\Exams"
   ```
3. **Network.** Start a session on your PC and check the practice exam shows *Connected*.
4. **Arabic.** Questions, options, names and answers can be in Arabic: letters join, lines read
   right-to-left, and mixed Arabic and English keeps the right order, both while typing and in
   the exam.

## Ports

| Port | Protocol | Purpose |
|---|---|---|
| 47810 | UDP | Students find the teacher (broadcast) |
| 47811 | TCP | Encrypted exam session (configurable per exam) |
| 47880 | TCP | Download page for the exam `.exe` |

Students only make outgoing connections, so their PCs need no firewall changes. On the
teacher's PC, **Start session** checks the firewall and, the first time, asks Windows for
permission (an all-users install of the Studio adds the rule during setup instead). The rule is
equivalent to:

```powershell
netsh advfirewall firewall add rule name="HDJLabs Studio" dir=in action=allow program="C:\path\to\HDJLabs.exe" enable=yes profile=any
```

If the network is marked **Public**, Windows may still block it. Switch it to **Private** in the
network's properties.

## What you see while it runs

* **Ready to start** checklist: network adapters, whether the exam is exported, firewall.
* **Exam open, not signed in**: PCs that have the exam open and found you.
* **Downloads**: each PC's download with a progress bar and speed.
* The roster: who is working, progress, time left, alerts and live answers.

Students see the same stages on their sign-in screen: *Looking for your teacher…* →
*Teacher found · 192.168.1.20* → *Connecting…* → *Connected*. If anything fails, the reason
is shown on hover and their result is still saved to a file.

## Handing out the exam

1. Export the exam (*Settings & export → Export .exe*).
2. *Live classroom → Start session*. The page shows an address like `http://192.168.1.20:47880/`.
3. Students open it in any browser and download the exam. A USB stick or shared folder works too;
   it's one file.

## If a student can't connect

* The exam keeps working offline, and the result is saved to `Documents\HDJLabs Results` and next
  to the exam file. Collect the `.hdjr` files and use *Results → Import result files*.
* On the student's sign-in screen, **Teacher address** lets them type the IP (or internet link)
  shown on your Live screen. That bypasses broadcast discovery, which some managed switches block.
* Exams try the teacher's IP addresses from export time automatically, so exporting on the same
  network you'll teach on helps.

## Students outside the classroom (internet link)

In a running session, press **Create internet link**. HDJLabs fetches Cloudflare's free tunnel
tool once (`cloudflared`, into `%LOCALAPPDATA%\HDJLabs`) and shows a public link such as
`https://some-words.trycloudflare.com`. Send it to the students: they open it in a browser,
download the exam and run it. The exam file is updated to include the link, so it connects back
by itself; a student with an older copy can type the link under **Teacher address**.

* Nothing is opened on your router and no account is needed. The link lasts until you stop it or
  the session; a new session gets a new link.
* The session inside the link is the same end-to-end encrypted protocol as on the LAN: Cloudflare
  carries it but can't read or change answers. The exam itself still needs the exam password
  (unless you let students start without it, below).
* Already reachable from the internet (public IP with port 47880 forwarded, your own domain or
  tunnel)? Use **Use my own address** instead.
* Students on the classroom network still connect directly; the link is only used when the
  teacher's PC isn't found nearby.

## Starting without the password

In a running session, tick **Students start without the password**. Every exam that is open on a
signed-out PC and connected to your session gets the exam password from your Studio over the
encrypted connection, so students only type their name and press **Start exam**. It's off by
default and lasts for that session only; untick it to require the password again.

* The password is encrypted on the network, and the exam only accepts it from your Studio. While
  the option is on, anyone with the exam file who reaches your session can open it, just as a
  student would; with it off, the file alone still reveals nothing.
* Studio first checks that the exported exam opens with the current password. If you changed it
  after exporting, export again.
* A PC that isn't connected to your session (offline, wrong network) still asks for the password.

## Running the exam in shifts

When a student has submitted and left, the next one can sit at the same PC without restarting
anything. In the Live classroom, click the student's name and press **Restart for next student**,
or press **Restart finished PCs** to clear every PC whose student has submitted. The exam goes
back to the sign-in screen, still locked, and the next student signs in with the same password.
The previous student's result stays in your Results and in the sealed `.hdjr` backup on that PC.

## Cleaning up

*Live classroom → Remove exam from PCs* deletes the exam file from every connected PC when its
exam closes (students who already submitted get a 10-second countdown). Or tick *Delete the exam
file from each student's PC after they submit* in Settings — that works offline too.
