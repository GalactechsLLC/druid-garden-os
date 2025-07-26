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

###################### Prepare First-Run Wizard #########################
echo "Configuring Armbian first-login defaults"

# Allow SSH even while firstlogin runs (remove PAM lock)
sed -i '/pam_nologin.so/d' /etc/pam.d/sshd

# Preset first-login answers so no interaction is needed
cat > /root/.not_logged_in_yet <<EOF
SET_LANG_BASED_ON_LOCATION="n"
PRESET_LOCALE="en_US.UTF-8"
PRESET_TIMEZONE="America/Los_Angeles"

# Root password (wizard will hash this)
PRESET_ROOT_PASSWORD="dgos123!"

# Networking defaults
PRESET_NET_CHANGE_DEFAULTS="1"
PRESET_NET_ETHERNET_ENABLED="1"
PRESET_NET_WIFI_ENABLED="1"

# Create druid user with password
PRESET_USER_NAME="druid"
PRESET_USER_PASSWORD="dgos123!"
PRESET_DEFAULT_REALNAME="Druid Garden"

# Skip unnecessary prompts but still run wizard to apply users/passwords
PRESET_CONNECT_WIRELESS="n"
SKIP_FIRST_LOGIN="no"
SKIP_ARMBIAN_PROMPT="yes"
ENABLED="yes"
EOF

chown root:root /root/.not_logged_in_yet
chmod 600 /root/.not_logged_in_yet

# Ensure firstlogin service is enabled so the wizard applies these presets
systemctl enable armbian-firstlogin.service || true
###################### Prepare First-Run Wizard End #########################

###################### Enable SSH #########################
echo "Enabling SSH login"
sed -i 's/^#\?PasswordAuthentication .*/PasswordAuthentication yes/' /etc/ssh/sshd_config
sed -i 's/^#\?PermitRootLogin .*/PermitRootLogin yes/' /etc/ssh/sshd_config
systemctl unmask ssh ssh.socket || true
systemctl enable ssh || true
systemctl restart ssh || true
###################### Enable SSH End #########################

###################### Set Up Docker #########################
echo "Installing / Setting up Docker."
chmod 1777 /tmp
curl -fsSL https://get.docker.com | sed 's/sleep 20/sleep 1/g' > /get-docker.sh
sh /get-docker.sh > /dev/null
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

###################### Force HDMI Output for Rock 4C #########################
if grep -q "rockpi-4cplus" /etc/armbian-release; then
    echo "Configuring forced HDMI mode for Rock 4C"
    sed -i '/^disp_mode=/d' /boot/armbianEnv.txt
    sed -i '/^extraargs=/d' /boot/armbianEnv.txt
    cat >> /boot/armbianEnv.txt <<'EOF'
disp_mode=1920x1080p60
extraargs=video=HDMI-A-1:1920x1080@60 video=HDMI-A-2:1920x1080@60 drm_kms_helper.edid_firmware=edid/1920x1080.bin
EOF
fi
###################### Force HDMI Output End #########################

###################### Final Cleanups #########################
if [ -d "/bin.usr-is-merged" ]; then
    rmdir /bin.usr-is-merged
fi
###################### Final Cleanups End #########################
