data_conf = configuration_data()
data_conf.set('APP_ID', application_id)
data_conf.set('APP_PATH', application_id.replace('.', '/'))
data_conf.set('bindir', get_option('prefix') / get_option('bindir'))

# Desktop file — configure @APP_ID@ then merge translations.
desktop_configured = configure_file(
  input: 'io.github.justinf555.Moments.desktop.in',
  output: application_id + '.desktop.in.in',
  configuration: data_conf
)
desktop_file = i18n.merge_file(
        input: desktop_configured,
       output: application_id + '.desktop',
         type: 'desktop',
       po_dir: '../po',
      install: true,
  install_dir: get_option('datadir') / 'applications'
)

desktop_utils = find_program('desktop-file-validate', required: false)
if desktop_utils.found()
  test('Validate desktop file', desktop_utils, args: [desktop_file])
endif

# Metainfo — configure @APP_ID@ then merge translations.
metainfo_configured = configure_file(
  input: 'io.github.justinf555.Moments.metainfo.xml.in',
  output: application_id + '.metainfo.xml.in.in',
  configuration: data_conf
)
appstream_file = i18n.merge_file(
        input: metainfo_configured,
       output: application_id + '.metainfo.xml',
       po_dir: '../po',
      install: true,
  install_dir: get_option('datadir') / 'metainfo'
)

appstreamcli = find_program('appstreamcli', required: false, disabler: true)
test('Validate appstream file', appstreamcli,
     args: ['validate', '--no-net', '--explain', appstream_file])

# GSettings schema — configure @APP_ID@ and @APP_PATH@.
gschema_file = configure_file(
  input: 'io.github.justinf555.Moments.gschema.xml.in',
  output: application_id + '.gschema.xml',
  configuration: data_conf
)
install_data(gschema_file,
  install_dir: get_option('datadir') / 'glib-2.0' / 'schemas'
)

compile_schemas = find_program('glib-compile-schemas', required: false, disabler: true)
test('Validate schema file',
     compile_schemas,
     args: ['--strict', '--dry-run', meson.current_build_dir()])

# D-Bus service file.
configure_file(
  input: 'io.github.justinf555.Moments.service.in',
  output: application_id + '.service',
  configuration: data_conf,
  install_dir: get_option('datadir') / 'dbus-1' / 'services'
)

subdir('icons')
