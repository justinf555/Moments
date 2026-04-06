pkgdatadir = get_option('prefix') / get_option('datadir') / meson.project_name()
gnome = import('gnome')

blueprints = custom_target(
  'blueprints',
  input: files(
    'shortcuts-dialog.blp',
    'ui/window.blp',
    'ui/import_dialog.blp',
    'ui/setup_window.blp',
    'ui/setup_window/backend_picker_page.blp',
    'ui/setup_window/local_setup_page.blp',
    'ui/setup_window/immich_setup_page.blp',
    'ui/viewer.blp',
    'ui/video_viewer.blp',
    'ui/people_grid.blp',
    'ui/album_grid.blp',
    'ui/photo_grid.blp',
    'ui/viewer/edit_panel.blp',
    'ui/photo_grid/cell.blp',
    'ui/album_grid/card.blp',
    'ui/people_grid/cell.blp',
  ),
  output: '.',
  command: [
    find_program('blueprint-compiler'),
    'batch-compile', '@OUTPUT@', '@CURRENT_SOURCE_DIR@',
    '@INPUT@',
  ],
)

gnome.compile_resources('moments',
  'moments.gresource.xml',
  gresource_bundle: true,
  install: true,
  install_dir: pkgdatadir,
  dependencies: blueprints,
)

conf = configuration_data()
conf.set_quoted('VERSION', meson.project_version())
conf.set_quoted('GETTEXT_PACKAGE', 'moments')
conf.set_quoted('LOCALEDIR', get_option('prefix') / get_option('localedir'))
conf.set_quoted('PKGDATADIR', pkgdatadir)
conf.set_quoted('APP_ID', application_id)
conf.set_quoted('PROFILE', profile)

configure_file(
    input: 'config.rs.in',
    output: 'config.rs',
    configuration: conf
)

# Copy the config.rs output to the source directory.
run_command(
  'cp',
  meson.project_build_root() / 'src' / 'config.rs',
  meson.project_source_root() / 'src' / 'config.rs',
  check: true
)

cargo_bin  = find_program('cargo')
cargo_opt  = [ '--manifest-path', meson.project_source_root() / 'Cargo.toml' ]
cargo_opt += [ '--target-dir', meson.project_build_root()  / 'src' ]

cargo_env  = [ 'CARGO_HOME=' + meson.project_build_root()  / 'cargo-home' ]

if get_option('buildtype') == 'release'
  cargo_opt += [ '--release' ]
  rust_target = 'release'
else
  rust_target = 'debug'
endif


cargo_build = custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  output: meson.project_name(),
  console: true,
  install: true,
  install_dir: get_option('bindir'),
  command: [
    'env', cargo_env,
    cargo_bin, 'build',
    cargo_opt, '&&', 'cp', 'src' / rust_target / meson.project_name(), '@OUTPUT@',
  ]
)

# ── Test targets (used by CI via flatpak-builder --run-tests) ──────────

cargo_test_env = [
  'CARGO_HOME=' + meson.project_build_root() / 'cargo-home',
  'SQLX_OFFLINE=true',
]

test('fmt',
  find_program('env'),
  args: [
    cargo_test_env,
    'cargo', 'fmt',
    '--manifest-path', meson.project_source_root() / 'Cargo.toml',
    '--', '--check',
  ],
  timeout: 60,
  suite: 'lint',
)

test('clippy',
  find_program('env'),
  args: [
    cargo_test_env,
    'cargo', 'clippy',
    cargo_opt,
    '--all-targets',
    '--', '-D', 'warnings',
  ],
  depends: cargo_build,
  timeout: 600,
  suite: 'lint',
)

test('unit-tests',
  find_program('env'),
  args: [
    cargo_test_env,
    'cargo', 'test',
    cargo_opt,
  ],
  depends: cargo_build,
  timeout: 600,
  suite: 'unit',
)

test('integration-tests',
  find_program('env'),
  args: [
    cargo_test_env,
    'cargo', 'test',
    cargo_opt,
    '--features', 'integration-tests',
    '--', '--test-threads=1',
  ],
  depends: cargo_build,
  timeout: 900,
  suite: 'integration',
)
