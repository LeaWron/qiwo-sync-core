// SPDX-License-Identifier: BSD-3-Clause
// Shared native export-only operation. Caller has verified input idle, released
// all session/database references, and serializes it with import/deployment.
#ifndef QIWO_NATIVE_RIME_SYNC_H
#define QIWO_NATIVE_RIME_SYNC_H
#include <rime_api.h>
#include <rime_levers_api.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#ifdef _WIN32
#include <windows.h>
#endif

// Small shared startup hint: pending downloaded configuration must not be
// silently compiled on restart. Broken/unreadable hints fail closed.
static inline int qiwo_rime_has_pending_deployment(RIME_FLAVORED(RimeApi)* api, const char* root) {
  const char* suffix = "/.qiwo-sync/deploy-hint.json";
  if (!root || !api) return True;
  size_t length = strlen(root) + strlen(suffix) + 1;
  char* path = (char*)malloc(length);
  if (!path) return True;
  strcpy(path, root); strcat(path, suffix);
  FILE* file;
#ifdef _WIN32
  int size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, 0, 0);
  wchar_t* wide = size > 0 ? (wchar_t*)malloc((size_t)size * sizeof(wchar_t)) : 0;
  if (!wide) { free(path); return True; }
  MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide, size);
  file = _wfopen(wide, L"rb"); free(wide);
#else
  file = fopen(path, "rb");
#endif
  int error = errno; free(path);
  if (!file) return error == ENOENT ? False : True;
  char json[4097]; size_t bytes = fread(json, 1, sizeof(json) - 1, file);
  int too_large = fgetc(file) != EOF || ferror(file); fclose(file); json[bytes] = 0;
  if (too_large || !api->config_load_string || !api->config_get_bool || !api->config_close) return True;
  RimeConfig config = {0}; Bool pending = True;
  if (!api->config_load_string(&config, json)) return True;
  Bool valid = api->config_get_bool(&config, "hasPendingDeploy", &pending);
  api->config_close(&config);
  return valid ? pending : True;
}

// Offline retries after a restart also need the deployer task registrations.
// Loading modules is separate from starting a workspace deployment.
static inline int qiwo_rime_start_snapshot_merge(RIME_FLAVORED(RimeApi)* api) {
  if (!api || !RIME_API_AVAILABLE(api, sync_user_data)) return False;
  if (RIME_API_AVAILABLE(api, deployer_initialize)) api->deployer_initialize(NULL);
  return api->sync_user_data() != 0;
}

static inline int qiwo_rime_export_own_snapshots(RIME_FLAVORED(RimeApi)* api) {
  if (!api || !RIME_API_AVAILABLE(api, run_task) ||
      !RIME_API_AVAILABLE(api, cleanup_all_sessions) ||
      !RIME_API_AVAILABLE(api, find_module)) return False;
  api->cleanup_all_sessions();
  // A normal input engine may not have loaded the deployer/lever modules yet.
  // This loads task registrations; it does not start workspace compilation.
  if (RIME_API_AVAILABLE(api, deployer_initialize)) api->deployer_initialize(NULL);
  if (!api->run_task("installation_update") ||
      !api->run_task("backup_config_files")) return False;
  RimeModule* module = api->find_module("levers");
  if (!module || !module->get_api) return False;
  RIME_FLAVORED(RimeLeversApi)* levers = (RIME_FLAVORED(RimeLeversApi)*)module->get_api();
  if (!levers || !RIME_API_AVAILABLE(levers, backup_user_dict) ||
      !levers->user_dict_iterator_init || !levers->next_user_dict ||
      !levers->user_dict_iterator_destroy) return False;
  RimeUserDictIterator iterator = {0};
  if (!levers->user_dict_iterator_init(&iterator)) return False;
  Bool success = True;
  const char* name;
  while ((name = levers->next_user_dict(&iterator)) != 0) {
    if (name[0] == '.') continue; // interrupted restore's temporary database
    if (!levers->backup_user_dict(name)) success = False;
  }
  levers->user_dict_iterator_destroy(&iterator);
  return success;
}
#endif
