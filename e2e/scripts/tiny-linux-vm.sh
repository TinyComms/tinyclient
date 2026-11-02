#!/bin/sh
# Creates the tiny-linux desktop guest on Atomic Studio, next to gunmetal-winbuild.
# Run on the Unraid host: ssh atomicstudio 'sh -s' < e2e/scripts/tiny-linux-vm.sh
set -eu
DIR=/mnt/system_cache_ssd/domains/tiny-linux
BASE=/mnt/system_cache_ssd/domains/teamcity-worker-01/ubuntu-base.img
IP=192.168.1.55
KEY='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPK0tEr+/AVbEww+QXYX4OjjxsYWtsYwtGxHu/ZP7+ou tinycomms'

if [ "${1:-}" != "rebuild" ] && virsh dominfo tiny-linux >/dev/null 2>&1; then
  echo "tiny-linux is already defined"
  virsh start tiny-linux >/dev/null 2>&1 || true
  virsh domstate tiny-linux
  exit 0
fi
if virsh dominfo tiny-linux >/dev/null 2>&1; then
  virsh destroy tiny-linux >/dev/null 2>&1 || true
  virsh undefine tiny-linux
fi

if ping -c 1 -W 1 "$IP" >/dev/null 2>&1; then
  echo "$IP already answers; refusing to claim it" >&2
  exit 1
fi

gw=$(ip -4 route show default | awk '{print $3; exit}')
if [ -z "$gw" ]; then
  echo "no default gateway on the host" >&2
  exit 1
fi
echo "gateway $gw"

grep -a -m 1 -o 'PRETTY_NAME="[^"]*"' "$BASE" || echo "base image name is not stored in cleartext; using ubuntu-base.img"

mkdir -p "$DIR"
if [ ! -f "$DIR/disk.qcow2" ] || [ "${1:-}" = "rebuild" ]; then
  rm -f "$DIR/disk.qcow2"
  qemu-img convert -O qcow2 "$BASE" "$DIR/disk.qcow2"
  qemu-img resize "$DIR/disk.qcow2" 80G
fi

cat > "$DIR/user-data" <<EOF
#cloud-config
hostname: tiny-linux
manage_etc_hosts: true
ssh_pwauth: false
users:
  - name: tinycomms
    groups: [adm, sudo]
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    lock_passwd: true
    ssh_authorized_keys:
      - ${KEY}
package_update: true
packages:
  - qemu-guest-agent
  - ubuntu-desktop-minimal
  - at-spi2-core
  - curl
  - ca-certificates
  - xz-utils
  - build-essential
write_files:
  - path: /etc/gdm3/custom.conf
    content: |
      [daemon]
      WaylandEnable=false
      AutomaticLoginEnable=true
      AutomaticLogin=tinycomms
      DefaultSession=ubuntu-xorg.desktop
  - path: /etc/dconf/profile/user
    content: |
      user-db:user
      system-db:local
  - path: /etc/dconf/db/local.d/00-tiny-e2e
    content: |
      [org/gnome/desktop/screensaver]
      lock-enabled=false
      [org/gnome/desktop/session]
      idle-delay=uint32 0
      [org/gnome/desktop/lockdown]
      disable-lock-screen=true
  - path: /usr/local/bin/tiny-e2e-unlock
    permissions: "0755"
    content: |
      #!/bin/sh
      export DISPLAY=:0
      gsettings set org.gnome.desktop.screensaver lock-enabled false || true
      gsettings set org.gnome.desktop.session idle-delay 0 || true
      xset s off || true
      xset -dpms || true
  - path: /home/tinycomms/.config/autostart/tiny-unlock.desktop
    owner: tinycomms:tinycomms
    defer: true
    content: |
      [Desktop Entry]
      Type=Application
      Name=Keep the desk unlocked
      Exec=/usr/local/bin/tiny-e2e-unlock
      X-GNOME-Autostart-enabled=true
runcmd:
  - usermod -aG audio,video,render tinycomms || usermod -aG audio,video tinycomms
  - dconf update
  - systemctl enable qemu-guest-agent
  - systemctl disable systemd-networkd systemd-networkd-wait-online || true
  - systemctl set-default graphical.target
  - apt-get install -y wmctrl x11-xserver-utils build-essential
  - curl -fsSL https://nodejs.org/dist/v22.21.0/node-v22.21.0-linux-x64.tar.xz -o /tmp/node.tar.xz
  - tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1
power_state:
  mode: reboot
  condition: true
EOF

cat > "$DIR/meta-data" <<EOF
instance-id: tiny-linux-1
local-hostname: tiny-linux
EOF

cat > "$DIR/network-config" <<EOF
version: 2
renderer: NetworkManager
ethernets:
  enp1s0:
    addresses:
      - ${IP}/24
    routes:
      - to: default
        via: ${gw}
    nameservers:
      addresses: [1.1.1.1, 8.8.8.8]
EOF

# A FAT label cannot be the lowercase cidata name cloud-init looks for. The seed is an ISO.
docker run --rm -v "$DIR:/work" -w /work ubuntu:24.04 \
  bash -lc 'apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq xorriso >/dev/null && xorriso -as mkisofs -o seed.iso -V cidata -J -R user-data meta-data network-config'

uuid=$(uuidgen)
cat > "$DIR/tiny-linux.xml" <<EOF
<domain type='kvm'>
  <name>tiny-linux</name>
  <uuid>${uuid}</uuid>
  <description>Tiny call e2e Linux desktop</description>
  <metadata>
    <vmtemplate xmlns="unraid" name="Ubuntu" icon="ubuntu.png" os="linux"/>
  </metadata>
  <memory unit='GiB'>16</memory>
  <currentMemory unit='GiB'>16</currentMemory>
  <vcpu placement='static'>8</vcpu>
  <os>
    <type arch='x86_64' machine='pc-q35-10.2'>hvm</type>
    <boot dev='hd'/>
  </os>
  <features>
    <acpi/>
    <apic/>
  </features>
  <cpu mode='host-passthrough' check='none'>
    <topology sockets='1' dies='1' cores='8' threads='1'/>
  </cpu>
  <clock offset='utc'/>
  <on_poweroff>destroy</on_poweroff>
  <on_reboot>restart</on_reboot>
  <on_crash>restart</on_crash>
  <devices>
    <emulator>/usr/local/sbin/qemu</emulator>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2' cache='writeback' discard='unmap'/>
      <source file='${DIR}/disk.qcow2'/>
      <target dev='vda' bus='virtio'/>
    </disk>
    <disk type='file' device='cdrom'>
      <driver name='qemu' type='raw'/>
      <source file='${DIR}/seed.iso'/>
      <target dev='sda' bus='sata'/>
      <readonly/>
    </disk>
    <interface type='bridge'>
      <mac address='52:54:00:74:6c:01'/>
      <source bridge='br0'/>
      <model type='virtio'/>
    </interface>
    <serial type='file'>
      <source path='${DIR}/serial.log'/>
      <target type='isa-serial' port='0'/>
    </serial>
    <console type='file'>
      <source path='${DIR}/serial.log'/>
      <target type='serial' port='0'/>
    </console>
    <channel type='unix'>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
    </channel>
    <input type='tablet' bus='usb'/>
    <graphics type='vnc' port='-1' autoport='yes' websocket='-1' listen='0.0.0.0' keymap='en-us'>
      <listen type='address' address='0.0.0.0'/>
    </graphics>
    <video>
      <model type='virtio' heads='1' primary='yes'>
        <acceleration accel3d='no'/>
      </model>
    </video>
    <memballoon model='virtio'/>
    <rng model='virtio'>
      <backend model='random'>/dev/urandom</backend>
    </rng>
  </devices>
</domain>
EOF

virsh define "$DIR/tiny-linux.xml"
virsh start tiny-linux
virsh domstate tiny-linux
echo "defined tiny-linux at ${IP} via ${gw}"
