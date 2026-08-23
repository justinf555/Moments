# SPDX-FileCopyrightText: 2026 Justin F
# SPDX-License-Identifier: GPL-3.0-or-later

project('moments', 'rust', 
          version: '0.4.2',
    meson_version: '>= 1.0.0',
  default_options: [ 'warning_level=2', 'werror=false', ],
)

i18n = import('i18n')
gnome = import('gnome')

# Release channels. Each gets its own application id so all three can be
# installed side by side: the id drives the GSettings path, data dir, keyring
# entry, desktop file, D-Bus name and icon name.
base_id = 'io.github.justinf555.Moments'
profile = get_option('profile')
if profile == 'development'
  application_id = base_id + '.Devel'
  application_name = 'Moments (Development)'
elif profile == 'nightly'
  application_id = base_id + '.Nightly'
  application_name = 'Moments (Nightly)'
else
  application_id = base_id
  application_name = 'Moments'
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
