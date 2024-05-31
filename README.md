nixless-agent is a NixOS machine updater for machines that don't want Nix (and its surface area).
It pulls the new system configuration from a binary cache, which works differently from most other updaters which push the new configuration into another machine instead.

nixless-agent is intended to be used in a server scenario, ideally one where you don't want operators that have SSH+root access to the machine to push new system configurations, even if those operators are automated instead of being human.
Because of this, nixless-agent doesn't require SSH access.
Updates are triggered by sending a signed request to an HTTP endpoint (the signature is how the agent understands what is a valid request).

At the moment, this is in pre-alpha stage, and is only tested inside NixOS VM tests.

Major features missing:

- Expose metrics that can be monitored from the outside of the host.
- Perform management and cleanup of old system configurations.

Plenty of minor features are missing as well, but those are too much to be listed here right now.

Note: currently this work is not licensed.
I want to get the code in a more robust state and finish some of the major features before licensing it.
If you happen to stumble onto this project and want to use it, please contact me so I can understand if you need anything else that I haven't planned, and so I can be more incentivised to finish the code faster :)

---

# Misc stuff that needs to find another place to live

Helpful tips when hacking on D-Bus stuff:

- You can sniff traffic with `busctl`.
  Example usage: `sudo busctl monitor --match "type='method_call',interface='org.freedesktop.systemd1.Manager'"`
  The match rule syntax is at https://dbus.freedesktop.org/doc/dbus-specification.html#message-bus-routing-match-rules
- The `StartTransientUnit` call on the systemd bus is very poorly documented.
  You can use [systemd-run](https://www.freedesktop.org/software/systemd/man/latest/systemd-run.html) together with a D-Bus traffic sniffer to figure out what exactly should be the arguments to the call.
  Once you run something with `systemd-run`, it will give you an invocation id, and if the service exits quickly, you won't be able to get a lot of info through systemctl.
  You can still view the logs by running something like `journalctl _SYSTEMD_INVOCATION_ID="<invocation id here>"`.
  It might be needed to run `systemd-run` without being root, because that was the only way I found to see the method call through a sniffer like `busctl`.

  The properties you can set on a transient service are documented [here](https://systemd.io/TRANSIENT-SETTINGS/).
  More details on the type of each property (for services) is [here](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html#Properties2).
  Beware that these types include runtime information, and for `StartTransientUnit` you may not need to pass those.
  Make sure to sniff the D-Bus traffic with `systemd-run` to figure out the proper types.

  Some more docs on transient services are [here](https://systemd.io/CONTROL_GROUP_INTERFACE/#creating-and-starting).

  Sometimes it may be necessary to reset the state of a failed transient service with `systemctl reset-failed <unit-name>` to get it to be removed.

- D-Bus types are documented [here](https://dbus.freedesktop.org/doc/dbus-specification.html#type-system).

Helpful tips when hacking on polkit stuff:

- You can log stuff from polkit's JavaScript API.
  Check if your polkit daemon was started with the `--no-debug` flag, because if it was, you won't be able to see the logs.
  Remove that flag and restart the daemon.

## Terminology

Nix store dir: usually the `/nix/store` dir.

Nix var dir: usually the `/nix/var` dir.

State base dir: the base directory where the nixless agent will keep its state. Currently this is the same as the Nix var dir.

Package: unit of reference in the Nix store dir. Corresponds to the output of a derivation. The Nix manual refers to this as a "store object", but this project chooses to use the "store object" term to mean any piece inside the Nix store dir (this could mean some file inside a package in the Nix store dir as well).

Package id: the hash + name of the package, e.g. `032wiarm65zp3bh9ak3dz2sqcr3n8g70-bash-interactive-5.2p26`.

Package path: the full path to the package in the filesystem. Is the result of joining the Nix store dir and the package id.
