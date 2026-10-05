# Set up dl-nzb on a Mac

## What you need

- A Mac on macOS 26 or later.
- A Usenet account: the server, username and password from your provider.
- An NZB file.

## 1. Install dl-nzb

1. Download the DMG from [Releases](https://github.com/zephleggett/dl-nzb/releases/latest).
2. Open it and drag **dl-nzb** to **Applications**.
3. Open **dl-nzb**.

You see **Connect to Your Usenet Server**.

## 2. Connect your server

1. Enter the **Host**, **Username** and **Password** from your provider.
2. Leave **Port** at **563** and **Use SSL/TLS** on, unless your provider says otherwise.
3. Set **Connections** to your provider's limit. The default is 30.
4. Click **Test Connection** and wait for a green **Connected**.
5. Click **Continue**.

Use the dl-nzb command line? Click **Import from dl-nzb CLI…**, then **Import**.
The fields fill in from its config; click **Continue**.

## 3. Download

Add an NZB any of these ways:

- Double-click it in Finder.
- Drop it on the window or the Dock icon.
- Click **Add NZB** in the toolbar, or choose **File > Open…**.

It joins the list and starts when its turn comes. dl-nzb downloads, repairs
with PAR2 and extracts archives by itself.

## 4. Find the files

Wait for a green check and **8.2 GB · Finished in 3 min**. Files are in
**Downloads**, one folder per download. Double-click the row to show them in Finder.

## What you see

| Status line | Means |
| --- | --- |
| **Waiting · 8.2 GB** | In the list; starts when its turn comes. |
| **Checking 21,840 articles…** | Asking the server what it has. |
| **3.1 GB of 8.2 GB · 84 MB/s · 1 min left** | Downloading. |
| **Downloading recovery data · 140 MB of 400 MB** | Fetching PAR2 data to fill gaps. |
| **Repairing 12 damaged blocks · 43%** | Fixing the files with PAR2. |
| **Extracting · 2 of 5** | Unpacking archives. |
| **8.2 GB · Finished in 3 min · Repaired 12 blocks** | Done; PAR2 fixed it. |
| **Paused · 3.1 GB of 8.2 GB** | Click **Resume** on the row to continue. |
| **Stopped** | **Retry** continues if you chose **Stop and Keep Data**. |

The window's subtitle shows the total, such as **2 downloading · 84 MB/s**.
The Dock icon shows a progress bar and how many are unfinished. Finder shows
progress on the download's folder. When a download finishes, a notification
offers **Show in Finder**. Turn on **Show in menu bar** in **Settings > General**
to see progress there too.

Closing the window keeps downloads going; click the Dock icon to bring it
back. Quitting asks first. Unfinished downloads continue next time you open
dl-nzb.

## Troubleshooting

| Problem | First step |
| --- | --- |
| **Couldn’t Log In to the Server** | Click **Open Settings** and fix **Username** and **Password**. Downloads resume by themselves. |
| **Server Not Found** or **Couldn’t Reach the Server** | Check **Host** and **Port** in **Settings > Server**, then click **Test Connection**. |
| **Needs Attention**: articles are missing | The server lacks too much to repair. Click **Remove**, or **Download Anyway** to keep what it has. |
| **Password Required** | Click **Enter Password…**, type the password, then click **Extract**. dl-nzb asks only if the NZB has no working password. |
| **Not Enough Space** | Free up disk space, then click **Retry**. |
| Download stops while the Mac sleeps | Turn on **Prevent sleep while downloading** in **Settings > General**. Closing a laptop's lid still sleeps it. |
| Double-click opens another app | Control-click the NZB and choose **Open With > dl-nzb**. |

Logs: **Settings > Advanced > Show Logs** opens them in Console. Attach them
to an [issue](https://github.com/zephleggett/dl-nzb/issues).

<details>
<summary>Optional settings</summary>

## Settings

Open **dl-nzb > Settings…**.

| Setting | Use |
| --- | --- |
| Download folder | Where downloads go. **Choose…** picks another folder. |
| Start downloads automatically | Off: new downloads wait until you click **Start**. |
| Remove finished downloads | **Manually**, **When dl-nzb quits** or **After one day**. Files stay. |
| Notify when downloads finish | One notification per download. |
| Prevent sleep while downloading | Keeps the Mac awake while anything downloads. On by default. |
| Show in menu bar | Progress and **Pause All** in the menu bar. |
| Check for updates automatically | Look for new versions. **Check Now** looks now. |
| Repair with PAR2, Extract archives | Fix and unpack downloads. On by default. |
| Delete archives after extracting, Delete PAR2 files after repairing | Keep only the finished files. Off by default. |
| Rename obfuscated files | Give scrambled file names their real names. |
| Check availability before downloading | Ask the server what it has first. **Automatic** does so only for NZBs without PAR2 files. |
| Download all recovery files up front | Otherwise fetched only when something is missing. |
| Limit download speed | Cap the speed at a **Maximum speed** in MB/s. |
| Verify server certificate, Retry attempts | Leave on and at 2 unless your provider says otherwise. |
| Flush files to disk when finished | Guards finished files against a power cut. Slower. |
| Reset All Settings | Back to defaults. Also removes the server password. |

**Pause All** and **Resume All** are in the **Downloads** menu and the Dock
icon's menu. Shortcuts can run **Download NZB**, **Pause All Downloads** and
**Resume All Downloads**.

</details>

<details>
<summary>Files and uninstalling</summary>

## Where things live

| What | Where |
| --- | --- |
| Downloads | `~/Downloads`, or your chosen download folder |
| List and settings | `~/Library/Containers/com.zephleggett.dl-nzb` |
| Server password | Keychain Access item `dl-nzb (your server)` |
| App | `/Applications/dl-nzb.app` |

To uninstall, click **Reset All Settings** in **Settings > Advanced**; this
removes the server password. Quit dl-nzb, then delete the app and
`~/Library/Containers/com.zephleggett.dl-nzb`. Your downloads stay where they
are.

</details>
