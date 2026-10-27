# microG and push

microG (GmsCore, GsfProxy and the FakeStore companion) runs on the CI image as
privileged system apps, layered in through `out/system_ext-overlay`. Location,
Google checkin and microG's push connection work. Apps that use the Play
Services client libraries don't, because AOSP 17 can't spoof signatures. For
push, UnifiedPush works today, with no microG at all.

Tested 2026-10-27 on `ci-16644040` with GmsCore 0.3.16.1, GsfProxy 0.1.0 and
FakeStore 0.3.16.1.

## Install

```sh
tools/microg.sh            # latest stable release
tools/microg.sh --beta     # include the Beta channel
tools/microg.sh --remove   # take it out of the overlay
```

Restart the session afterwards. The script downloads into `out/microg/` and
writes these files under `out/system_ext-overlay/`:

- `priv-app/{GmsCore,GsfProxy,FakeStore}/<Name>.apk`.
- `priv-app/GmsCore/lib/x86_64/*.so`. GmsCore's native libraries are compressed,
  and PackageManager never extracts them for a system app
  (`PackageAbiHelperImpl.shouldExtractLibs`), so the script unpacks them.
- `etc/permissions/privapp-permissions-microg.xml`.
- `etc/default-permissions/default-permissions-microg.xml`: runtime grants
  (location, accounts, notifications, phone state, storage). They apply on the
  first boot of a fresh `/data`, and the user can revoke them.
- `etc/sysconfig/microg.xml`: GmsCore is exempt from battery optimizations and
  Data Saver.

The XMLs live in `tools/microg/etc/`. No session change is needed.

**Verification:** microG's published key has SHA-256
`9BD06727E62796C0130EB6DAB39B73157451582CBD138E86C468ACC395D14165` (GmsCore
wiki, Downloads).

1. The repo's `entry.jar` must be signed by that key.
2. `entry.json` pins `index-v2.json` by SHA-256.
3. The index pins each APK by SHA-256.
4. `apksigner` must show that the same key signs each APK.

The index's `signer` field for GmsCore and FakeStore is `f0fd6c5b…`, the SHA-256
of Google's certificate, so the script ignores that field.

## Privileged permissions

The image has `ro.control_privapp_permissions=enforce`, so system_server won't
boot unless every `signature|privileged` permission a priv-app requests is
listed. We listed them from `aapt2 dump permissions` checked against the image's
`framework-res.apk`. All of them are granted:

- GmsCore: `CHANGE_DEVICE_IDLE_TEMP_WHITELIST`, `DUMP`,
  `INSTALL_LOCATION_PROVIDER`, `INTERACT_ACROSS_USERS`, `LOCATION_HARDWARE`,
  `MANAGE_USB`, `MODIFY_PHONE_STATE`, `NETWORK_SCAN`,
  `PROVIDE_DEFAULT_ENABLED_CREDENTIAL_SERVICE`, `PROVIDE_REMOTE_CREDENTIALS`,
  `START_ACTIVITIES_FROM_BACKGROUND`, `UPDATE_APP_OPS_STATS`,
  `UPDATE_DEVICE_STATS`, `WATCH_APPOPS`.
- FakeStore: `DELETE_PACKAGES`, `INSTALL_PACKAGES`, `INTERACT_ACROSS_USERS`.
- GsfProxy: none.

`FAKE_PACKAGE_SIGNATURE`, which microG also requests, isn't defined on AOSP 17,
so it doesn't matter here.

Installed as user apps with `pm install`, microG gets none of these permissions.
It also can't become the location provider, which must be a system package.
GsfProxy targets SDK 23, so it would need `--bypass-low-target-sdk-block`. We
didn't test this path, because the priv-app path works.

## What works without spoofing

- **Boot:** the session boots normally, with all three apps installed as
  `PRIVILEGED SYSTEM_EXT`.
- **Location:** `dumpsys location` shows the `network` and `fused` providers
  bound to `com.google.android.gms` (microG's `NetworkLocationProviderService`
  and `FusedLocationProviderService`). No framework overlay is needed, because
  `config_enableNetworkLocationOverlay` and `config_enableFusedLocationOverlay`
  default to true, so any system package can be the provider. We got no network
  fix: the session has no Wi-Fi or cell scans for microG to look up.
- **Checkin:** with checkin turned on in microG's settings, it registered with
  Google and got an `androidId`.
- **Push connection:** with Cloud Messaging turned on, `McsService` logged in to
  `mtalk.google.com:5228` (`LoginResponse … Logged in`), and the TCP connection
  stayed `ESTABLISHED`.
- **Self-check:** packages installed, location, accounts, notifications and
  phone permissions granted, and battery optimizations ignored. These fail:
  "System spoofs signature", and "has correct signature" for GmsCore and
  FakeStore. SMS and draw-over-apps aren't granted either (not needed here).

To turn on checkin and Cloud Messaging without the UI (as root):

```sh
content update --uri content://com.google.android.gms.microg.settings/check-in \
  --bind checkin_enable_service:i:1
content update --uri content://com.google.android.gms.microg.settings/gcm \
  --bind gcm_enable_mcs_service:b:true
```

## What doesn't work without spoofing

The Play Services client libraries, and Google's servers, check that
`com.google.android.gms` and `com.android.vending` carry Google's certificate
(SHA-1 `38:91:8A:45…:57:88`). microG's carry microG's. So these fail:

- FCM/GCM for apps built with Firebase or Play Services: the library reports the
  service as invalid and never registers;
- Google sign-in through the client library, and the Maps API;
- SafetyNet and Play Integrity (these also fail on any test-keys image);
- Play licensing through FakeStore.

## Signature spoofing

AOSP 17 has no spoofing hook. `frameworks/base` has no `FAKE_PACKAGE_SIGNATURE`
or `fake-signature` code, and no config, overlay, sysconfig or permission
changes the signatures PackageManager reports. They come only from the APK
(`ParsingPackageUtils.getSigningDetails`) or the copy cached in `packages.xml`.
Editing `packages.xml` offline would also do it, but we ruled that out: it
changes Android's state behind its back, like a hook.

**The smallest patch.** In
`services/core/java/com/android/server/pm/ComputerEngine.java`,
`generatePackageInfo()`, after `PackageInfoUtils.generate(...)` returns, add
about 40 lines that set `packageInfo.signatures` (for `GET_SIGNATURES`) and
`packageInfo.signingInfo` (for `GET_SIGNING_CERTIFICATES`) to the certificate in
the app's `fake-signature` meta-data. That is the hex DER string microG already
ships in GmsCore and FakeStore. Spoof only when all of these hold:

1. The package is `com.google.android.gms` or `com.android.vending`.
2. It's a preinstalled system app (`ps.isSystem()`), not an update from `/data`.
3. Its real signer is microG's certificate (SHA-256 `9bd06727…d14165`).
4. The `fake-signature` value is exactly Google's certificate (SHA-1
   `38918a45…ced5788`, hard-coded).

This restricted form only lets microG pose as Google, and nobody else can, so it
needs no new permission or user toggle. CalyxOS and /e/OS do the same.
Permission grants and `checkSignatures` keep using the real `SigningDetails`, so
only what apps read changes. The patch belongs in the image lane's patch queue
for our source-built image (M4). The CI image can't carry it.

## UnifiedPush

UnifiedPush gives push without Google: a distributor app holds one connection to
a push server, and apps register with it. microG isn't a UnifiedPush
distributor. ntfy (F-Droid `io.heckel.ntfy` 1.25.2) is one, and it works in the
session:

1. Install ntfy and UP-Example (`org.unifiedpush.example` 2.3.1) from F-Droid
   with `pm install -g`, and open ntfy once.
2. UP-Example registers with ntfy, the only distributor, by itself. It shows its
   endpoint, for example `https://ntfy.sh/upKkjnBg95zCrJ?up=1`.
3. ntfy keeps a JSON-stream connection to ntfy.sh in a foreground service
   ("Listening for incoming notifications").
4. From the host, `curl -d "hi from the host" '<endpoint>'` sent the message.
   ntfy passed it to UP-Example, which posted a notification ("UP-Example: hi
   from the host" in `dumpsys notification`). The library falls back to plain
   text when a message isn't encrypted.
5. UP-Example's "Send notification" button sends an encrypted (RFC 8291) WebPush
   message through the same path, and it arrived too.

Notes:

- At first, Android blocked ntfy's subscriber service ("startForegroundService()
  not allowed"), because ntfy was in the background when the registration came
  in. With ntfy exempt from battery optimizations
  (`cmd deviceidle whitelist +io.heckel.ntfy`, or ntfy's own prompt), it starts
  from the background. A preinstalled distributor would get this from a
  sysconfig `allow-in-power-save` entry, like GmsCore's.
- The image doesn't enable Doze (`dumpsys deviceidle force-idle`: "not
  enabled"), so a running session doesn't throttle the connection.
- Push only arrives while the session runs. ntfy.sh holds messages for 12 hours,
  and ntfy fetches them when it reconnects.
