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

###################### Set Up User Password #########################
echo "Setting up Root User"
echo root:${PI_PASSWORD} | chpasswd
###################### Set Up User Password End #####################

###################### Set Up Docker #########################
echo "Installing / Setting up Docker."
chmod 1777 /tmp
curl -fsSL https://get.docker.com | sed 's/sleep 20/sleep 1/g' > get-docker.sh
sh get-docker.sh > /dev/null
systemctl enable docker.service > /dev/null || true
systemctl start docker.service > /dev/null || true
###################### Set Up Docker End #####################

###################### Set Up System Service #########################
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
###################### Set Up System Service End #####################

cat > /root/.not_logged_in_yet <<EOF
# Skip auto-locale detection by GPS/IP
SET_LANG_BASED_ON_LOCATION="n"

# System
PRESET_LOCALE="en_US.UTF-8"
PRESET_TIMEZONE="America/Los_Angeles"

# Root password (must supply to skip prompt)
PRESET_ROOT_PASSWORD="dgos123!"

# Networking — supply something so it won’t ask
PRESET_NET_CHANGE_DEFAULTS="1"
PRESET_NET_ETHERNET_ENABLED="1"
PRESET_NET_WIFI_ENABLED="1"

# User creation
PRESET_USER_NAME="druid"
PRESET_USER_PASSWORD="dgos123!"
PRESET_DEFAULT_REALNAME="Druid Garden"

# Skip interactive prompts
PRESET_CONNECT_WIRELESS="n"
EOF

if [ -d "/bin.usr-is-merged"]; then
  rmdir /bin.usr-is-merged
fi

if [ -f "/get-docker.sh"]; then
  rm get-docker.sh
fi