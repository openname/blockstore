# Sample init scripts and service configuration for stacks-blockchain

Sample scripts and configuration files for systemd and SysVinit
can be found in the [contrib/init](../contrib/init) folder.

    contrib/init/stacks.service:    systemd service unit configuration
    contrib/init/stacks.init:       SysV style init script

## Service User

All Linux startup configurations assume the existence of a "stacks" user
and group. They must be created before attempting to use these scripts.
The MacOS configuration assumes stacks-blockchain will be set up for the current user.

## Configuration

For an example configuration file that describes the configuration settings, see [mainnet-follower-conf.toml](../sample/conf/mainnet-follower-conf.toml).
Available configuration options are [documented here](https://docs.stacks.co/stacks-in-depth/nodes-and-miners/stacks-node-configuration).

## Paths

### Linux

All three configurations assume several paths that might need to be adjusted.

    Binary:              /usr/local/bin/stacks-node
    Configuration file:  /etc/stacks-blockchain/Config.toml
    Data directory:      /stacks-blockchain
    PID file:            /run/stacks-blockchain/stacks.pid
    Lock file:           /var/lock/subsys/stacks (SysVinit)

The PID directory and data directory should both be owned by the
stacks user and group. It is advised for security reasons to make the
configuration file and data directory only readable by the stacks user and
group.

NOTE: When using the systemd .service file, the creation of the aforementioned
directories and the setting of their permissions is automatically handled by
systemd. Directories are given a permission of 710, giving the stacks group
access to files under it _if_ the files themselves give permission to the
stacks group to do so. This does not allow for the listing of files under the directory.

```bash
$ mkdir -p /etc/stacks-blockchain/
$ mkdir -p /stacks-blockchain
$ useradd stacks
$ chown -R stacks:stacks /stacks-blockchain/
$ chgrp -R stacks /etc/stacks-blockchain
```

### macOS

    Binary:              /usr/local/bin/stacks-node
    Configuration file:  /etc/stacks-blockchain/Config.toml

The Config.toml file is presumed to have group ownership by the `wheel` group, with the current user having membership to that group. The data directory defined in the config is required to be writable by the current user. Logs (stdout & stderr) are saved under `/tmp/stacks-blockchain.log`

## Installing Service Configuration

### systemd

Installing this .service file consists of just copying it to
/usr/lib/systemd/system directory, followed by the command
`systemctl daemon-reload` in order to update running systemd configuration.

To test, run `systemctl start stacks` and to enable for system startup run
`systemctl enable stacks`

NOTE: When installing for systemd in Debian/Ubuntu the .service file needs to be copied to the /lib/systemd/system directory instead.

### SysVinit

Copy stacks.init to /etc/init.d/stacks. Test by running `service stacks start`.

Using this script, you can adjust the config path and log location to the stacks-node program by
setting the STACKS_BLOCKCHAIN_CONFIG and STACKS_BLOCKCHAIN_LOG environment variables in the file
/etc/sysconfig/stacks-blockchain.

### MacOS

Copy org.stacks.stacks-blockchain.plist into ~/Library/LaunchAgents. Load the launch agent by
running `launchctl load ~/Library/LaunchAgents/org.stacks.stacks-blockchain.plist`.

This Launch Agent will **not** start the stacks-blockchain whenever the user logs in.

To start the service, you'll need to manually run the start command: `launchctl start org.stacks.stacks-blockchain`

NOTE: This approach is intended for those wanting to run stacks-blockchain as the current user.
You will need to modify org.stacks.stacks-blockchain.plist if you intend to use it as a
Launch Daemon with a dedicated stacks user.

## Auto-respawn

Auto respawning is currently disabled.
