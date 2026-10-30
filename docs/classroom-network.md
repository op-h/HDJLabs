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

## Ports

| Port | Protocol | Purpose |
|---|---|---|
| 47810 | UDP | Students find the teacher (broadcast) |
| 47811 | TCP | Encrypted exam session (configurable per exam) |
| 47880 | TCP | Download page for the exam `.exe` |

Students only make outgoing connections, so their PCs need no firewall changes. On the
teacher's PC, click **Allow through firewall** in *Live classroom* once. It adds an inbound rule
for HDJLabs (Windows asks for administrator permission). The equivalent command is:

```powershell
netsh advfirewall firewall add rule name="HDJLabs Studio" dir=in action=allow program="C:\path\to\HDJLabs.exe" enable=yes profile=any
```

If the network is marked **Public**, Windows may still block it. Switch it to **Private** in the
network's properties.

## Handing out the exam

1. Export the exam (*Settings & export → Export .exe*).
2. *Live classroom → Start session*. The page shows an address like `http://192.168.1.20:47880/`.
3. Students open it in any browser and download the exam. A USB stick or shared folder works too;
   it's one file.

## If a student can't connect

* The exam keeps working offline, and the result is saved to `Documents\HDJLabs Results` and next
  to the exam file. Collect the `.hdjr` files and use *Results → Import result files*.
* On the student's sign-in screen, **Teacher address** lets them type the IP shown on your Live
  screen. That bypasses broadcast discovery, which some managed switches block.
* Exams try the teacher's IP addresses from export time automatically, so exporting on the same
  network you'll teach on helps.
