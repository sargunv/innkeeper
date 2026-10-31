# microg

Optional microG (GmsCore, GsfProxy, FakeStore) as privileged system apps.
`mise run microg` (`install.sh`; `-- --beta`, `-- --remove`) downloads it from
microG's F-Droid repo, verifies it (the trust chain is in the script's header)
and installs it into the addons layer
`$XDG_DATA_HOME/innkeeper/addons/microg/system_ext`, never into the stage; the
next session start stacks it over `/system_ext`. `etc/` holds the privapp
permissions (`ro.control_privapp_permissions=enforce` refuses to boot without
them), default runtime grants and a battery-optimization exemption.

- **Works:** boot, microG as the network and fused location provider, Google
  checkin and the Cloud Messaging connection.
- **Doesn't:** anything built on the Play Services client libraries (FCM for
  Firebase apps, Google sign-in, Maps API, licensing), because they check that
  GmsCore carries Google's certificate, and AOSP 17 has no signature-spoofing
  hook. Innkeeper does not support signature spoofing: framework patches and
  runtime hooks cross our unmodified-image boundary. These clients remain
  unsupported under microG
  ([decision #23](https://github.com/sargunv/innkeeper/issues/23)).
- **Push without Google:** use a UnifiedPush distributor with apps that support
  UnifiedPush; it does not make FCM clients work. No distributor is shipped. If
  you install one, such as ntfy, exempt it from Android's battery optimisation
  so it can start its service from the background.

For ntfy (`io.heckel.ntfy`), grant the exemption in a running session:

```sh
flatpak run --command=/app/libexec/innkeeper/innkeeper-session dev.sargunv.Innkeeper \
  exec dumpsys deviceidle whitelist +io.heckel.ntfy
```

Use `exec dumpsys deviceidle whitelist` to check the list; ntfy should appear as
`user,io.heckel.ntfy,...`. For another distributor, substitute its package name.
The exemption persists across session restarts; remove it with
`exec dumpsys deviceidle whitelist -io.heckel.ntfy`.
