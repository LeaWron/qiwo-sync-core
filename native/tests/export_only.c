#include "../rime_sync.h"
#include <assert.h>

static unsigned cleanups, imports, backups, installation, config_backups;
static Bool backup_fail;
static RimeLeversApi levers;
static RimeModule module;
static void cleanup(void) { ++cleanups; }
static Bool sync_all(void) { ++imports; return True; }
static Bool task(const char* name) {
  if (!strcmp(name, "installation_update")) ++installation;
  else if (!strcmp(name, "backup_config_files")) ++config_backups;
  else assert(0);
  return True;
}
static Bool iterator_init(RimeUserDictIterator* iterator) { iterator->i = 0; return True; }
static void iterator_destroy(RimeUserDictIterator* iterator) { (void)iterator; }
static const char* next(RimeUserDictIterator* iterator) {
  const char* names[] = {"main", ".temp", "second", 0};
  return names[iterator->i++];
}
static Bool backup(const char* name) { assert(name[0] != '.'); ++backups;return !backup_fail; }
static RimeCustomApi* get_levers(void) { return (RimeCustomApi*)&levers; }
static RimeModule* find(const char* name) { return !strcmp(name, "levers") ? &module : 0; }
int main(void) {
  RimeApi api = {0}; RIME_STRUCT_INIT(RimeApi, api);
  RIME_STRUCT_INIT(RimeLeversApi, levers); RIME_STRUCT_INIT(RimeModule, module);
  api.cleanup_all_sessions = cleanup;api.sync_user_data = sync_all;api.run_task = task;api.find_module = find;
  module.get_api = get_levers;
  levers.user_dict_iterator_init = iterator_init;levers.user_dict_iterator_destroy = iterator_destroy;
  levers.next_user_dict = next;levers.backup_user_dict = backup;
  assert(qiwo_rime_export_own_snapshots(&api));
  assert(cleanups == 1 && imports == 0 && backups == 2 && installation == 1 && config_backups == 1);
  backup_fail = True;assert(!qiwo_rime_export_own_snapshots(&api));
  assert(imports == 0);
  module.get_api = 0;assert(!qiwo_rime_export_own_snapshots(&api));
  return 0;
}
