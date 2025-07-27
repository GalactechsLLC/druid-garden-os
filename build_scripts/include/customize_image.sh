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

###################### Set Passwords Directly #########################
echo "Setting up users and passwords directly"

# Set root password directly (don't rely on first-login wizard)
echo "root:dgos123!" | chpasswd

# Create druid user and set password
useradd -m -s /bin/bash -G sudo,adm,dialout,cdrom,floppy,audio,video,plugdev,netdev druid 2>/dev/null || true
echo "druid:dgos123!" | chpasswd

# Ensure home directory exists with correct permissions
mkdir -p /home/druid
chown druid:druid /home/druid
chmod 755 /home/druid
###################### Set Passwords Directly End #########################

###################### Skip First-Login Wizard Completely #########################
echo "Disabling Armbian first-login wizard..."

# Remove the first-login trigger file
rm -f /root/.not_logged_in_yet

# Create the completion marker that Armbian checks for
touch /root/.armbian_first_login_check_complete

# Disable the first-login service completely
systemctl disable armbian-firstlogin.service 2>/dev/null || true
systemctl mask armbian-firstlogin.service 2>/dev/null || true

# Remove first-login related PAM restrictions
sed -i '/pam_nologin.so/d' /etc/pam.d/sshd
sed -i '/pam_nologin.so/d' /etc/pam.d/login

# Set locale and timezone directly to avoid prompts
echo 'LANG=en_US.UTF-8' > /etc/default/locale
echo 'America/Los_Angeles' > /etc/timezone
ln -sf /usr/share/zoneinfo/America/Los_Angeles /etc/localtime
locale-gen en_US.UTF-8

# Mark system as configured
touch /etc/armbian-image-release
echo "ARMBIAN_IMAGE_CONFIGURED=yes" >> /etc/armbian-image-release

echo "First-login wizard disabled ✓"
###################### Skip First-Login Wizard End #########################

###################### Enable SSH #########################
echo "Enabling SSH login"
sed -i 's/^#\?PasswordAuthentication .*/PasswordAuthentication yes/' /etc/ssh/sshd_config
sed -i 's/^#\?PermitRootLogin .*/PermitRootLogin yes/' /etc/ssh/sshd_config
sed -i 's/^#\?ChallengeResponseAuthentication .*/ChallengeResponseAuthentication no/' /etc/ssh/sshd_config
sed -i 's/^#\?UsePAM .*/UsePAM yes/' /etc/ssh/sshd_config

systemctl unmask ssh ssh.socket || true
systemctl enable ssh || true
###################### Enable SSH End #########################

###################### Set Up Docker #########################
echo "Installing / Setting up Docker."
chmod 1777 /tmp
curl -fsSL https://get.docker.com | sed 's/sleep 20/sleep 1/g' > /get-docker.sh
sh /get-docker.sh > /dev/null

# Add druid user to docker group
usermod -aG docker druid 2>/dev/null || true

systemctl enable docker.service > /dev/null || true
systemctl start docker.service > /dev/null || true
rm -f /get-docker.sh
###################### Set Up Docker End #########################

###################### Set Up System Services #########################
echo "[Unit]
Description=Run Druid Garden Updater Service
After=NetworkManager.service
Requires=NetworkManager.service

[Service]
Type=oneshot
ExecStart=/usr/bin/druid-garden-edge-updater.app
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target" > /etc/systemd/system/druid_garden_edge_updater.service
systemctl enable druid_garden_edge_updater.service > /dev/null || true

echo "[Unit]
Description=Run Druid Garden OS
After=NetworkManager.service
Requires=NetworkManager.service

[Service]
ExecStart=/usr/bin/druid-garden-os.app

[Install]
WantedBy=multi-user.target" > /etc/systemd/system/druid_garden_os.service
systemctl enable druid_garden_os.service > /dev/null || true
systemctl disable dnsmasq > /dev/null || true
systemctl daemon-reload > /dev/null || true
###################### Set Up System Services End #########################

###################### Verify Configuration #########################
echo "Verifying user configuration..."
id druid || echo "ERROR: druid user not created"
getent passwd root || echo "ERROR: root user issue"

echo "Verifying SSH configuration..."
sshd -t || echo "ERROR: SSH configuration invalid"

echo "User setup complete:"
echo "  Root password: dgos123!"
echo "  Druid user password: dgos123!"
echo "  SSH enabled with password authentication"
echo "  First-login wizard disabled"
###################### Verify Configuration End #########################

###################### Final Cleanups #########################
if [ -d "/bin.usr-is-merged" ]; then
    rmdir /bin.usr-is-merged
fi
###################### Final Cleanups End #########################