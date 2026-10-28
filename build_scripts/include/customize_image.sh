#!/usr/bin/env bash
set -euo pipefail
###################### Copy Overlay Files to ROOT #########################
cp -r /tmp/overlay/* /
mv /druid-garden-edge-updater.app /usr/bin/druid-garden-edge-updater.app
mv /druid-garden-os.app /usr/bin/druid-garden-os.app
chmod +x /usr/bin/druid-garden-edge-updater.app
chmod +x /usr/bin/druid-garden-os.app
###################### Copy Overlay Files to ROOT END #####################

###################### Install System Deps #########################
apt update -y
DEBIAN_FRONTEND=noninteractive apt install curl dnsmasq iw ntfs-3g ntpdate -y
DEBIAN_FRONTEND=noninteractive apt upgrade -y
###################### Install System Deps End #####################

# No shared credentials are baked into the image. Local first-login provisioning
# and PAM restrictions remain intact.
if ! id druid >/dev/null 2>&1; then
    useradd -m -s /bin/bash -G sudo,adm,dialout,cdrom,audio,video,plugdev,netdev druid
fi
passwd --lock root
passwd --lock druid
install -d -m 0755 -o druid -g druid /home/druid

# Put policy first: sshd uses the first value for each setting, including Includes.
# Disable keyboard-interactive as well as password auth, and require public keys.
ssh_policy=$(mktemp)
cat > "$ssh_policy" <<'SSH_POLICY'
PermitRootLogin no
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
PubkeyAuthentication yes
AuthenticationMethods publickey
UsePAM yes
SSH_POLICY
cat /etc/ssh/sshd_config >> "$ssh_policy"
install -m 0600 "$ssh_policy" /etc/ssh/sshd_config
rm -f "$ssh_policy"

# Optional build input: DG_SSH_AUTHORIZED_KEYS_FILE. Only public keys belong here.
if [[ -s /druid-authorized-keys ]]; then
    ssh-keygen -l -f /druid-authorized-keys >/dev/null
    install -d -m 0700 -o druid -g druid /home/druid/.ssh
    install -m 0600 -o druid -g druid /druid-authorized-keys /home/druid/.ssh/authorized_keys
    # Allow the authenticated provisioning operator to retrieve only the initial
    # web password; locked Unix passwords otherwise prevent ordinary sudo use.
    install -d -m 0750 /etc/sudoers.d
    echo 'druid ALL=(root) NOPASSWD: /usr/bin/cat /var/lib/druid-garden/bootstrap-password' > /etc/sudoers.d/druid-bootstrap
    chmod 0440 /etc/sudoers.d/druid-bootstrap
    visudo -cf /etc/sudoers.d/druid-bootstrap
    systemctl unmask ssh.service ssh.socket
    systemctl enable ssh.service
else
    # Enable SSH only after an administrator explicitly provisions public keys.
    systemctl disable ssh.service
    systemctl mask ssh.service ssh.socket
fi
rm -f /druid-authorized-keys

# Use distribution-signed packages instead of executing a remote install script.
DEBIAN_FRONTEND=noninteractive apt-get install --no-install-recommends -y docker.io
# Docker access is root-equivalent; the SSH provisioning account does not need it.
gpasswd -d druid docker >/dev/null 2>&1 || true
systemctl enable docker.service

###################### Set Up System Services #########################
echo "[Unit]
Description=Run Druid Garden Updater Service
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/bin/druid-garden-edge-updater.app
UMask=0077
TimeoutStartSec=15min

[Install]
WantedBy=multi-user.target" > /etc/systemd/system/druid_garden_edge_updater.service
cat > /etc/systemd/system/druid_garden_edge_updater.timer <<'UPDATE_TIMER'
[Unit]
Description=Check for signed Druid Garden OS updates

[Timer]
OnBootSec=15min
OnUnitActiveSec=6h
RandomizedDelaySec=15min
Unit=druid_garden_edge_updater.service

[Install]
WantedBy=timers.target
UPDATE_TIMER
systemctl disable druid_garden_edge_updater.service
cat > /etc/systemd/system/druid_garden_update_recovery.service <<'UPDATE_RECOVERY'
[Unit]
Description=Recover interrupted Druid Garden OS application updates
Before=druid_garden_os.service

[Service]
Type=oneshot
ExecStart=/usr/bin/druid-garden-edge-updater.app --recover-only
UMask=0077
RemainAfterExit=yes
TimeoutStartSec=2min
UPDATE_RECOVERY
if [[ -s /druid-update-public-key ]]; then
    install -d -m 0755 -o root -g root /etc/druid-garden
    install -m 0644 -o root -g root /druid-update-public-key /etc/druid-garden/update-public-key
    update_public_key=$(tr -d '\r\n' < /druid-update-public-key)
    update_generation=$(cat /druid-update-key-generation)
    [[ "$update_public_key" =~ ^[[:xdigit:]]{64}$ ]]
    [[ "$update_generation" =~ ^(0|[1-9][0-9]{0,17})$ ]]
    printf '{"generation":%s,"public_key":"%s"}\n' "$update_generation" "$update_public_key" > /etc/druid-garden/update-trust.yaml
    chmod 0644 /etc/druid-garden/update-trust.yaml
    chown root:root /etc/druid-garden/update-trust.yaml
    systemctl enable druid_garden_edge_updater.timer > /dev/null
else
    systemctl disable druid_garden_edge_updater.timer
fi
rm -f /druid-update-public-key /druid-update-key-generation

echo "[Unit]
Description=Run Druid Garden OS
After=NetworkManager.service druid_garden_update_recovery.service
Requires=NetworkManager.service druid_garden_update_recovery.service

[Service]
ExecStart=/usr/bin/druid-garden-os.app
WorkingDirectory=/var/lib/druid-garden
StateDirectory=druid-garden
StateDirectoryMode=0700
UMask=0077
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target" > /etc/systemd/system/druid_garden_os.service
systemctl enable druid_garden_os.service > /dev/null
systemctl disable dnsmasq > /dev/null

###################### Set Up System Services End #########################

# Verify required provisioning commands succeeded without printing credentials.
id druid
getent passwd root

###################### Final Cleanups #########################
if [ -d "/bin.usr-is-merged" ]; then
    rmdir /bin.usr-is-merged
fi
###################### Final Cleanups End #########################
