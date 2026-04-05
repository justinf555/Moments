project('moments', 'rust', 
          version: '0.2.0',
    meson_version: '>= 1.0.0',
  default_options: [ 'warning_level=2', 'werror=false', ],
)

i18n = import('i18n')
gnome = import('gnome')

base_id = 'io.github.justinf555.Moments'
if get_option('profile') == 'development'
  application_id = base_id + '.Devel'
  profile = 'development'
else
  application_id = base_id
  profile = 'default'
endif


subdir('data')
subdir('src')
subdir('po')

install_data(
  'COPYING',
  install_dir: get_option('datadir') / 'licenses' / meson.project_name()
)

gnome.post_install(
     glib_compile_schemas: true,
    gtk_update_icon_cache: true,
  update_desktop_database: true,
)
