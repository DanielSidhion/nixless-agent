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
