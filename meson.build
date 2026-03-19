project(
    'libratbag',
    'rust',
    version: '1.3.0',
    license: ['GPL-3.0-or-later', 'MIT'],
    meson_version: '>= 0.58.0',
)

project_source_root = meson.project_source_root()

# --- Dependencies ---

# udev is required for device discovery
dependency('udev')

# Systemd integration
enable_systemd = get_option('systemd')
dep_systemd = dependency('systemd', required: enable_systemd)

# Cargo is required for building Rust components
cargo = find_program('cargo', required: true)
cargo_path = cargo.full_path()

# GUI Setup
build_gui = get_option('build-gui')
if build_gui
    npm = find_program('npm', required: true)
    npm_path = npm.full_path()

    # Tauri Linux dependencies
    # We check these early to fail fast if system libraries are missing
    dependency('webkit2gtk-4.0', required: true)
    dependency('gtk+-3.0', required: true)
    dependency('libsoup-2.4', required: false)
    dependency('appindicator3-0.1', required: false)
endif

# --- Source Paths ---
ratbagd_src = join_paths(project_source_root, 'ratbagd-rs')
ratbagctl_src = join_paths(project_source_root, 'ratbagctl-rs')
twister_src = join_paths(project_source_root, 'twister')

# --- Build Targets ---

# 1. ratbagd daemon
custom_target(
    'ratbagd',
    build_by_default: true,
    command: [
        'sh',
        '-c', 'cd "@0@" && "@1@" build --release --locked && cp target/release/ratbagd-rs "@2@"'.format(ratbagd_src, cargo_path, '@OUTPUT@'),
    ],
    output: 'ratbagd',
    install: true,
    install_dir: get_option('bindir'),
    console: true,
)

# 2. ratbagctl CLI
custom_target(
    'ratbagctl',
    build_by_default: true,
    command: [
        'sh',
        '-c', 'cd "@0@" && "@1@" build --release --locked && cp target/release/ratbagctl "@2@"'.format(ratbagctl_src, cargo_path, '@OUTPUT@'),
    ],
    output: 'ratbagctl',
    install: true,
    install_dir: get_option('bindir'),
    console: true,
)

# 3. Twister GUI
if build_gui
    # We use 'npm ci' if package-lock.json exists for deterministic builds, otherwise 'npm install'.
    # We also assume 'npx' is available in the path or installed via npm.
    gui_build_cmd = 'cd "@0@" && if [ -f package-lock.json ]; then "@1@" ci; else "@1@" install; fi && npx @tauri-apps/cli build && cp src-tauri/target/release/twister "@2@"'.format(twister_src, npm_path, '@OUTPUT@')

    custom_target(
        'twister',
        build_by_default: true,
        command: ['sh', '-c', gui_build_cmd],
        output: 'twister',
        install: true,
        install_dir: get_option('bindir'),
        console: true,
    )
endif

# --- Installation ---

# Man pages
install_man('ratbagd/ratbagd.8')

# Device database (.device files)
install_subdir(
    'data/devices',
    strip_directory: true,
    exclude_files: ['device.example', 'README.md'],
    install_dir: join_paths(get_option('datadir'), 'libratbag'),
)

# Configuration for file substitution
conf_data = configuration_data()
conf_data.set('sbindir', join_paths(get_option('prefix'), get_option('bindir')))

# Systemd Unit File
if enable_systemd
    unitdir = get_option('systemd-unit-dir')
    if unitdir == ''
        # Default to the system systemd directory (e.g. /lib/systemd/system)
        # This ensures the unit works out-of-the-box on most distros.
        # Users installing to a custom prefix should specify -Dsystemd-unit-dir explicitly.
        unitdir = dep_systemd.get_variable(pkgconfig: 'systemdsystemunitdir')
    endif

    configure_file(
        input: 'ratbagd/ratbagd.service.in',
        output: 'ratbagd.service',
        configuration: conf_data,
        install_dir: unitdir,
    )
endif

# DBus Configuration
# Use explicit option or derive from datadir
dbusdir = get_option('dbus-root-dir')
if dbusdir == ''
    # Join with datadir (e.g. 'share'). Meson handles prefixing automatically if path is relative.
    # This prevents double-prefixing issues.
    dbusdir = join_paths(get_option('datadir'), 'dbus-1')
endif

# DBus Service File
configure_file(
    input: 'dbus/org.freedesktop.ratbag1.service.in',
    output: 'org.freedesktop.ratbag1.service',
    configuration: conf_data,
    install_dir: join_paths(dbusdir, 'system-services'),
)

# DBus Policy File
dbusgroup = get_option('dbus-group')
if dbusgroup == ''
    access = 'context="default"'
else
    access = 'group="@0@"'.format(dbusgroup)
endif

conf_dbus_access = configuration_data()
conf_dbus_access.set('access', access)

configure_file(
    input: 'dbus/org.freedesktop.ratbag1.conf.in',
    output: 'org.freedesktop.ratbag1.conf',
    configuration: conf_dbus_access,
    install_dir: join_paths(dbusdir, 'system.d'),
)
