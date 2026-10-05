# Set up dl-nzb on iPhone or iPad

## What you need

- An iPhone on iOS 26 or later, or an iPad on iPadOS 26 or later.
- A Usenet account: the server, username and password from your provider.
- An NZB file.

## 1. Install dl-nzb

<!-- TODO(owner): replace XXXXXXXX with the public TestFlight code. -->

<a href="https://testflight.apple.com/join/XXXXXXXX"><img src="images/testflight-badge.svg" alt="Available on TestFlight" height="40"></a>

1. Open **[the dl-nzb beta](https://testflight.apple.com/join/XXXXXXXX)** on the iPhone or iPad.
2. Install **TestFlight** if asked, then tap **Accept** and **Install**.
3. Open **dl-nzb**.

You see **Connect to Your Usenet Server**.

## 2. Connect your server

1. Enter the **Host**, **Username** and **Password** from your provider.
2. Leave **Port** at **563** and **Use SSL/TLS** on, unless your provider says otherwise.
3. Set **Connections** to your provider's limit. The default is 20.
4. Tap **Test Connection** and wait for a green **Connected**.
5. Tap **Continue**.

## 3. Download

Add an NZB either way:

- Tap **Add NZB** and choose it.
- Share it to **dl-nzb** from **Files**, Safari or another app.

It appears in **Downloads** and starts when its turn comes. dl-nzb downloads,
repairs with PAR2 and extracts archives by itself. Allow notifications if asked.

The ring on the right shows progress; tap it to pause or resume. Swipe a row
for more, or tap it for details. On iPad, the list and the details sit side
by side.

## 4. Find the files

Wait for a green check and **8.2 GB · Finished in 3 min**. Files are in
**Files > On My iPhone > dl-nzb > Downloads**, one folder per download. On
iPad, it's **On My iPad**. **Show in Files** on a download opens its folder.

## Leaving the app

Downloads keep going for a while after you leave dl-nzb; the system shows
their progress. If the system ends that time, or you swipe its progress away,
downloads pause. They resume when you open dl-nzb again.

On cellular data, a personal hotspot or Low Data Mode, dl-nzb asks
**Download on Cellular?** first. Tap **Download**, or **Wait for Wi-Fi** to start
once you're on Wi-Fi.

## What you see

| Status line | Means |
| --- | --- |
| **Waiting · 8.2 GB** | In the list; starts when its turn comes. |
| **Checking 21,840 articles…** | Asking the server what it has. |
| **3.1 of 8.2 GB · 1 min left** | Downloading. The speed is at the top of the list. |
| **Recovery data · 140 of 400 MB** | Fetching PAR2 data to fill gaps. |
| **Repairing 12 damaged blocks · 43%** | Fixing the files with PAR2. |
| **Extracting · 2 of 5** | Unpacking archives. |
| **8.2 GB · Repaired 12 blocks** | Done; PAR2 fixed it. |
| **Paused · 3.1 of 8.2 GB** | Tap the ring to continue. |

## Troubleshooting

| Problem | First step |
| --- | --- |
| Build expired | Install the newest build in **TestFlight**. Builds expire after 90 days. |
| **Couldn’t Log In to the Server** | Tap **Open Settings** and fix **Username** and **Password**. Downloads resume by themselves. |
| **Server Not Found** or **Couldn’t Reach the Server** | Check **Host** and **Port** in **Settings**, then tap **Test Connection**. |
| **Needs Attention**: articles are missing | The server lacks too much to repair. Tap **Remove**, or **Download Anyway** to keep what it has. |
| **Password Required** | Tap **Enter Password…**, type the password, then tap **Extract**. dl-nzb asks only if the NZB has no working password. |
| **Not Enough Space** | Free up storage, then tap **Retry** (the circled arrow) on the row. |
| Downloads paused after you left the app | Open dl-nzb; they resume. Keep it open for long downloads. |
| **Waiting for Wi-Fi** | Join Wi-Fi, or turn on **Allow downloads on cellular** in **Settings**. |

Need help? Open an [issue](https://github.com/zephleggett/dl-nzb/issues).

<details>
<summary>Optional settings</summary>

## Settings

Tap the gear at the top left of **Downloads**.

| Setting | Use |
| --- | --- |
| Downloads | Where files go. **Show in Files** opens the folder. |
| Start downloads automatically | Off: new downloads wait until you tap **Start**. |
| Remove finished downloads | **Manually**, **When dl-nzb quits** or **After one day**. Files stay. |
| Notify when downloads finish | One notification per download. |
| Allow downloads on cellular | Download on cellular without asking. Off by default. |
| Repair with PAR2, Extract archives | Fix and unpack downloads. On by default. |
| Delete archives after extracting, Delete PAR2 files after repairing | Keep only the finished files. Off by default. |
| Rename obfuscated files | Give scrambled file names their real names. |
| Check availability before downloading | Ask the server what it has first. **Automatic** does so only for NZBs without PAR2 files. |
| Download all recovery files up front | Otherwise fetched only when something is missing. |
| Limit download speed | Cap the speed at a **Maximum speed** in MB/s. |
| Verify server certificate, Retry attempts | Leave on and at 2 unless your provider says otherwise. |
| Flush files to disk when finished | Guards finished files against a power cut. Slower. |
| Reset All Settings | Back to defaults. Also removes the server password. |

**Pause All**, **Resume All** and **Remove Finished** are in the **More** (**…**) menu.

</details>

<details>
<summary>Files and uninstalling</summary>

## Where things live

| What | Where |
| --- | --- |
| Downloads | **Files > On My iPhone > dl-nzb > Downloads** |
| List and settings | Inside the app |
| Server password | The device's keychain |

To uninstall, move any downloads you want to keep out of
**On My iPhone > dl-nzb**. Tap **Reset All Settings** to remove the server
password, then delete the app. Deleting the app deletes its downloads.

</details>
