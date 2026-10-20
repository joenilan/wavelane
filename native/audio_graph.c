/* Desktop audio graph: every output device and every application stream,
 * per-application routing and volume, and live levels for both.
 *
 * Applications are routed only by writing their stream's `target.object` in
 * the session manager's default metadata, exactly like a desktop volume
 * control; the session manager performs the move. Levels come from passive
 * capture streams (a sink's monitor, an application's own output), which
 * never keep a device awake and never save any audio.
 *
 * Ownership rules:
 * - Only proxies and streams created by this client are destroyed.
 * - Metadata writes target only Stream/Output/Audio nodes present in the
 *   current registry and not marked node.dont-move.
 * - The loop is a pw_thread_loop: every public entry point locks it.
 */
#include <errno.h>
#include <inttypes.h>
#include <math.h>
#include <pipewire/extensions/metadata.h>
#include <pipewire/pipewire.h>
#include <spa/param/audio/format-utils.h>
#include <spa/param/props.h>
#include <spa/pod/parser.h>
#include <spa/utils/json.h>
#include <spa/utils/string.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#if !PW_CHECK_VERSION(1, 0, 0)
#error PipeWire headers >= 1.0 are required
#endif

/* Marks this client's own nodes so they never appear as apps or devices. */
#define AG_OWNER "audiomap.meter"
/* Level windows published per second. */
#define AG_LEVEL_RATE 30

enum { AG_NODE = 1, AG_PORT, AG_LINK, AG_CLIENT, AG_DEVICE };

struct ag_object {
  uint32_t id, kind;
  uint64_t serial;
  struct pw_properties *props;
  struct pw_proxy *proxy; /* bound nodes and clients only */
  struct spa_hook listener;
  int state;
  bool info;
  /* links */
  uint32_t out_node, in_node;
  /* output streams and sinks: their own volume and mute */
  float volume;
  bool mute, level_known;
  uint32_t channels;
  struct ag_object *next;
};

/* Current `target.object` metadata value of one stream subject. */
struct ag_target {
  uint32_t subject;
  char value[256];
  struct ag_target *next;
};

/* A passive capture of one node's output, reduced to its peak. */
struct ag_meter {
  uint32_t node;
  uint64_t serial;
  bool sink;
  struct pw_stream *stream;
  struct spa_hook listener;
  /* Written on the realtime thread, read by snapshots. */
  _Atomic uint32_t peak_bits;
  _Atomic int64_t published_ms;
  float window_peak;
  uint32_t window_frames;
  bool seen; /* reconcile mark */
  struct ag_meter *next;
};

struct audio_graph {
  struct pw_thread_loop *loop;
  struct pw_context *context;
  struct pw_core *core;
  struct pw_registry *registry;
  struct spa_hook core_listener, registry_listener;
  struct pw_metadata *metadata;
  struct spa_hook metadata_listener;
  uint32_t metadata_id;
  struct ag_target *targets;
  char default_sink[256], configured_sink[256];
  struct ag_object *objects;
  struct ag_meter *meters;
  bool metering;
  bool broken;
  char error[256];
  int seq, done;
};

static int64_t ag_millis(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (int64_t)t.tv_sec * 1000 + t.tv_nsec / 1000000;
}
static uint64_t ag_num(const char *s) {
  if (!s || !*s) return 0;
  for (const char *p = s; *p; p++)
    if (*p < '0' || *p > '9') return 0;
  errno = 0;
  char *end;
  uint64_t n = strtoull(s, &end, 10);
  return errno || *end ? 0 : n;
}
static const char *ag_prop(struct ag_object *o, const char *key) {
  return o && o->props ? pw_properties_get(o->props, key) : NULL;
}
static void ag_fail(struct audio_graph *g, const char *fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(g->error, sizeof(g->error), fmt, ap);
  va_end(ap);
}
static struct ag_object *ag_find(struct audio_graph *g, uint32_t id) {
  for (struct ag_object *o = g->objects; o; o = o->next)
    if (o->id == id) return o;
  return NULL;
}
static bool ag_is_class(struct ag_object *o, const char *media_class) {
  return o && o->kind == AG_NODE && spa_streq(ag_prop(o, PW_KEY_MEDIA_CLASS), media_class);
}
/* An application's output stream (not ours). */
static bool ag_is_app_stream(struct ag_object *o) {
  return ag_is_class(o, "Stream/Output/Audio") && !ag_prop(o, AG_OWNER);
}
/* An output device (or virtual output) a stream can play to. */
static bool ag_is_output(struct ag_object *o) {
  return ag_is_class(o, "Audio/Sink") && !ag_prop(o, AG_OWNER);
}

/* ---------- metadata ---------- */
static void ag_targets_clear(struct audio_graph *g, uint32_t subject, bool all) {
  struct ag_target **slot = &g->targets;
  while (*slot) {
    if (all || (*slot)->subject == subject) {
      struct ag_target *t = *slot;
      *slot = t->next;
      free(t);
    } else {
      slot = &(*slot)->next;
    }
  }
}
/* The "name" of a default-device value such as {"name":"alsa_output.x"}. */
static void ag_json_name(const char *value, char *out, size_t size) {
  out[0] = '\0';
  if (!value) return;
  struct spa_json it[2];
  char key[16];
  spa_json_init(&it[0], value, strlen(value));
  if (spa_json_enter_object(&it[0], &it[1]) <= 0) return;
  while (spa_json_get_string(&it[1], key, sizeof(key)) > 0) {
    if (spa_streq(key, "name")) {
      if (spa_json_get_string(&it[1], out, (int)size) <= 0) out[0] = '\0';
      return;
    }
    const char *skip;
    if (spa_json_next(&it[1], &skip) <= 0) return;
  }
}
static int ag_metadata_property(void *data, uint32_t subject, const char *key,
                                const char *type, const char *value) {
  struct audio_graph *g = data;
  (void)type;
  if (subject == PW_ID_CORE) {
    if (!key || spa_streq(key, "default.audio.sink"))
      ag_json_name(key ? value : NULL, g->default_sink, sizeof(g->default_sink));
    if (!key || spa_streq(key, "default.configured.audio.sink"))
      ag_json_name(key ? value : NULL, g->configured_sink, sizeof(g->configured_sink));
    return 0;
  }
  if (!key) {
    ag_targets_clear(g, subject, false);
    return 0;
  }
  if (!spa_streq(key, "target.object")) return 0;
  ag_targets_clear(g, subject, false);
  if (value && strlen(value) < sizeof(((struct ag_target *)0)->value)) {
    struct ag_target *t = calloc(1, sizeof(*t));
    if (t) {
      t->subject = subject;
      snprintf(t->value, sizeof(t->value), "%s", value);
      t->next = g->targets;
      g->targets = t;
    }
  }
  return 0;
}
static const struct pw_metadata_events ag_metadata_events = {
    .version = PW_VERSION_METADATA_EVENTS, .property = ag_metadata_property};

/* ---------- registry ---------- */
static void ag_object_free(struct ag_object *o) {
  if (o->proxy) {
    spa_hook_remove(&o->listener);
    pw_proxy_destroy(o->proxy);
  }
  pw_properties_free(o->props);
  free(o);
}
static void ag_node_info(void *data, const struct pw_node_info *info) {
  struct ag_object *o = data;
  if (info->props) pw_properties_update(o->props, info->props);
  o->state = info->state;
  o->info = true;
}
/* Average channel volume and mute from a Props param; false when it is not
 * the main Props object. The volume is on the cubic scale desktop mixers
 * show (PipeWire's channelVolumes are linear amplitude): 50% here is 50% in
 * Plasma's volume applet and pavucontrol. */
static bool ag_parse_level(const struct spa_pod *param, float *volume, bool *mute,
                           uint32_t *channels) {
  struct spa_pod *volumes = NULL;
  bool m = *mute;
  if (spa_pod_parse_object(param, SPA_TYPE_OBJECT_Props, NULL,
                           SPA_PROP_channelVolumes, SPA_POD_OPT_Pod(&volumes),
                           SPA_PROP_mute, SPA_POD_OPT_Bool(&m)) < 0)
    return false;
  float values[SPA_AUDIO_MAX_CHANNELS];
  uint32_t n = volumes ? spa_pod_copy_array(volumes, SPA_TYPE_Float, values,
                                            SPA_AUDIO_MAX_CHANNELS) : 0;
  if (!n) return false;
  float sum = 0.f;
  for (uint32_t i = 0; i < n; i++) sum += isfinite(values[i]) ? values[i] : 0.f;
  *volume = cbrtf(sum / (float)n);
  *mute = m;
  *channels = n;
  return true;
}
static void ag_node_param(void *data, int seq, uint32_t id, uint32_t index,
                          uint32_t next, const struct spa_pod *param) {
  struct ag_object *o = data;
  (void)seq; (void)index; (void)next;
  if (id != SPA_PARAM_Props || !param) return;
  float volume = o->volume;
  bool mute = o->mute;
  uint32_t channels = 0;
  if (ag_parse_level(param, &volume, &mute, &channels)) {
    o->volume = volume;
    o->mute = mute;
    o->channels = channels;
    o->level_known = true;
  }
}
static const struct pw_node_events ag_node_events = {
    .version = PW_VERSION_NODE_EVENTS, .info = ag_node_info, .param = ag_node_param};
static void ag_client_info(void *data, const struct pw_client_info *info) {
  struct ag_object *o = data;
  if (info->props) pw_properties_update(o->props, info->props);
  o->info = true;
}
static const struct pw_client_events ag_client_events = {
    .version = PW_VERSION_CLIENT_EVENTS, .info = ag_client_info};

static void ag_global(void *data, uint32_t id, uint32_t permissions, const char *type,
                      uint32_t version, const struct spa_dict *props) {
  struct audio_graph *g = data;
  (void)permissions;
  if (spa_streq(type, PW_TYPE_INTERFACE_Metadata)) {
    if (!g->metadata && props &&
        spa_streq(spa_dict_lookup(props, PW_KEY_METADATA_NAME), "default")) {
      g->metadata = pw_registry_bind(g->registry, id, type,
                                     SPA_MIN(version, (uint32_t)PW_VERSION_METADATA), 0);
      g->metadata_id = id;
      if (g->metadata)
        pw_metadata_add_listener(g->metadata, &g->metadata_listener, &ag_metadata_events, g);
    }
    return;
  }
  uint32_t kind = spa_streq(type, PW_TYPE_INTERFACE_Node)     ? AG_NODE
                  : spa_streq(type, PW_TYPE_INTERFACE_Port)   ? AG_PORT
                  : spa_streq(type, PW_TYPE_INTERFACE_Link)   ? AG_LINK
                  : spa_streq(type, PW_TYPE_INTERFACE_Client) ? AG_CLIENT
                  : spa_streq(type, PW_TYPE_INTERFACE_Device) ? AG_DEVICE
                                                              : 0;
  if (!kind || !props || ag_find(g, id)) return;
  struct ag_object *o = calloc(1, sizeof(*o));
  if (!o) return;
  o->id = id;
  o->kind = kind;
  o->props = pw_properties_new_dict(props);
  if (!o->props) {
    free(o);
    return;
  }
  o->serial = ag_num(pw_properties_get(o->props, PW_KEY_OBJECT_SERIAL));
  if (kind == AG_LINK) {
    o->out_node = (uint32_t)ag_num(pw_properties_get(o->props, PW_KEY_LINK_OUTPUT_NODE));
    o->in_node = (uint32_t)ag_num(pw_properties_get(o->props, PW_KEY_LINK_INPUT_NODE));
  } else if (kind == AG_CLIENT) {
    /* Client info carries the process identity for streams that lack it. */
    o->proxy = pw_registry_bind(g->registry, id, type,
                                SPA_MIN(version, (uint32_t)PW_VERSION_CLIENT), 0);
    if (o->proxy) pw_proxy_add_object_listener(o->proxy, &o->listener, &ag_client_events, o);
  } else if (kind == AG_NODE) {
    const char *mc = pw_properties_get(o->props, PW_KEY_MEDIA_CLASS);
    if (mc && (spa_streq(mc, "Stream/Output/Audio") || spa_streq(mc, "Audio/Sink"))) {
      o->proxy = pw_registry_bind(g->registry, id, type,
                                  SPA_MIN(version, (uint32_t)PW_VERSION_NODE), 0);
      if (o->proxy) {
        pw_proxy_add_object_listener(o->proxy, &o->listener, &ag_node_events, o);
        uint32_t ids[] = {SPA_PARAM_Props};
        pw_node_subscribe_params((struct pw_node *)o->proxy, ids, 1);
      }
    }
  }
  o->next = g->objects;
  g->objects = o;
}
static void ag_meter_destroy(struct ag_meter *m);
static void ag_global_remove(void *data, uint32_t id) {
  struct audio_graph *g = data;
  struct ag_object **slot = &g->objects;
  while (*slot) {
    if ((*slot)->id == id) {
      struct ag_object *o = *slot;
      *slot = o->next;
      ag_object_free(o);
      break;
    }
    slot = &(*slot)->next;
  }
  ag_targets_clear(g, id, false);
  if (g->metadata && id == g->metadata_id) {
    spa_hook_remove(&g->metadata_listener);
    ag_targets_clear(g, 0, true);
    pw_proxy_destroy((struct pw_proxy *)g->metadata);
    g->metadata = NULL;
    g->metadata_id = 0;
    g->default_sink[0] = g->configured_sink[0] = '\0';
  }
}
static const struct pw_registry_events ag_registry_events = {
    .version = PW_VERSION_REGISTRY_EVENTS,
    .global = ag_global,
    .global_remove = ag_global_remove};

/* ---------- core ---------- */
static void ag_core_done(void *data, uint32_t id, int seq) {
  struct audio_graph *g = data;
  if (id == PW_ID_CORE) {
    g->done = seq;
    pw_thread_loop_signal(g->loop, false);
  }
}
static void ag_core_error(void *data, uint32_t id, int seq, int res, const char *message) {
  struct audio_graph *g = data;
  (void)seq;
  if (id == PW_ID_CORE && res == -EPIPE) {
    g->broken = true;
    ag_fail(g, "PipeWire connection lost: %s", message ? message : "");
  }
  pw_thread_loop_signal(g->loop, false);
}
static const struct pw_core_events ag_core_events = {
    .version = PW_VERSION_CORE_EVENTS, .done = ag_core_done, .error = ag_core_error};

/* Lock held. 0 when the server processed everything sent so far, -1 when the
 * connection is broken, -2 on a transient timeout. */
static int ag_sync(struct audio_graph *g) {
  if (g->broken) return -1;
  int seq = pw_core_sync(g->core, PW_ID_CORE, g->seq);
  if (seq < 0) return -2;
  g->seq = seq;
  int64_t deadline = ag_millis() + 3000;
  while (g->done != g->seq && !g->broken) {
    if (ag_millis() > deadline) return -2;
    struct timespec abstime;
    pw_thread_loop_get_time(g->loop, &abstime, 200 * SPA_NSEC_PER_MSEC);
    pw_thread_loop_timed_wait_full(g->loop, &abstime);
  }
  return g->broken ? -1 : 0;
}

/* ---------- levels ---------- */
static void ag_meter_process(void *data) {
  struct ag_meter *m = data;
  struct pw_buffer *b = pw_stream_dequeue_buffer(m->stream);
  if (!b) return;
  struct spa_buffer *buf = b->buffer;
  if (buf->n_datas > 0 && buf->datas[0].data && buf->datas[0].chunk) {
    const float *samples = (const float *)((uint8_t *)buf->datas[0].data +
                                           (buf->datas[0].chunk->offset % buf->datas[0].maxsize));
    uint32_t n = buf->datas[0].chunk->size / sizeof(float);
    if (n > buf->datas[0].maxsize / sizeof(float)) n = buf->datas[0].maxsize / sizeof(float);
    float peak = m->window_peak;
    for (uint32_t i = 0; i < n; i++) {
      float v = samples[i];
      if (!isfinite(v)) continue;
      float a = v < 0.f ? -v : v;
      if (a > peak) peak = a;
    }
    m->window_peak = peak;
    m->window_frames += n / 2;
  }
  pw_stream_queue_buffer(m->stream, b);
  /* About AG_LEVEL_RATE windows a second at any graph rate. */
  if (m->window_frames >= 48000 / AG_LEVEL_RATE) {
    uint32_t bits;
    memcpy(&bits, &m->window_peak, 4);
    atomic_store(&m->peak_bits, bits);
    atomic_store(&m->published_ms, ag_millis());
    m->window_peak = 0.f;
    m->window_frames = 0;
  }
}
static const struct pw_stream_events ag_meter_events = {
    .version = PW_VERSION_STREAM_EVENTS, .process = ag_meter_process};

static void ag_meter_destroy(struct ag_meter *m) {
  /* pw_stream_destroy disconnects from the data loop (synchronized with
   * the realtime callback) before releasing its hooks. */
  if (m->stream) pw_stream_destroy(m->stream);
  free(m);
}

/* Lock held. A passive capture of a sink's monitor, or of an application's
 * own output: stereo float, at the graph rate. */
static struct ag_meter *ag_meter_create(struct audio_graph *g, struct ag_object *node, bool sink) {
  struct ag_meter *m = calloc(1, sizeof(*m));
  if (!m) return NULL;
  m->node = node->id;
  m->serial = node->serial;
  m->sink = sink;
  char target[32];
  snprintf(target, sizeof(target), "%" PRIu64, node->serial);
  struct pw_properties *p = pw_properties_new(
      PW_KEY_MEDIA_TYPE, "Audio", PW_KEY_MEDIA_CATEGORY, "Monitor", PW_KEY_MEDIA_ROLE, "DSP",
      PW_KEY_NODE_NAME, "audiomap-level", PW_KEY_MEDIA_NAME, "Level meter",
      PW_KEY_APP_NAME, "Audiomap", PW_KEY_TARGET_OBJECT, target, PW_KEY_NODE_PASSIVE, "true",
      PW_KEY_NODE_DONT_RECONNECT, "true", "node.dont-move", "true", "state.restore-props",
      "false", "state.restore-target", "false", "node.hidden", "true", AG_OWNER, "true", NULL);
  if (!p) {
    free(m);
    return NULL;
  }
  if (sink) pw_properties_set(p, PW_KEY_STREAM_CAPTURE_SINK, "true");
  m->stream = pw_stream_new(g->core, "Audiomap level", p);
  if (!m->stream) {
    free(m);
    return NULL;
  }
  pw_stream_add_listener(m->stream, &m->listener, &ag_meter_events, m);
  uint8_t buffer[512];
  struct spa_pod_builder pb = SPA_POD_BUILDER_INIT(buffer, sizeof(buffer));
  struct spa_audio_info_raw info = {.format = SPA_AUDIO_FORMAT_F32, .channels = 2,
                                    .position = {SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR}};
  const struct spa_pod *params[1] = {
      spa_format_audio_raw_build(&pb, SPA_PARAM_EnumFormat, &info)};
  if (pw_stream_connect(m->stream, PW_DIRECTION_INPUT, PW_ID_ANY,
                        PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS |
                            PW_STREAM_FLAG_RT_PROCESS,
                        params, 1) < 0) {
    pw_stream_destroy(m->stream);
    free(m);
    return NULL;
  }
  return m;
}

/* Lock held. One meter per output and per playing application; meters whose
 * node went away (or stopped playing) are released. */
static void ag_meters_reconcile(struct audio_graph *g) {
  for (struct ag_meter *m = g->meters; m; m = m->next) m->seen = false;
  if (g->metering) {
    for (struct ag_object *o = g->objects; o; o = o->next) {
      bool sink = ag_is_output(o);
      bool app = ag_is_app_stream(o) && o->state == PW_NODE_STATE_RUNNING;
      if (!sink && !app) continue;
      struct ag_meter *found = NULL;
      for (struct ag_meter *m = g->meters; m && !found; m = m->next)
        if (m->node == o->id && m->serial == o->serial) found = m;
      if (!found) {
        found = ag_meter_create(g, o, sink);
        if (!found) continue;
        found->next = g->meters;
        g->meters = found;
      }
      found->seen = true;
    }
  }
  struct ag_meter **slot = &g->meters;
  while (*slot) {
    if (!(*slot)->seen) {
      struct ag_meter *m = *slot;
      *slot = m->next;
      ag_meter_destroy(m);
    } else {
      slot = &(*slot)->next;
    }
  }
}

/* ---------- public ---------- */
struct audio_graph *ag_open(void) {
  pw_init(NULL, NULL);
  struct audio_graph *g = calloc(1, sizeof(*g));
  if (!g) return NULL;
  g->done = -1;
  g->loop = pw_thread_loop_new("audiomap-graph", NULL);
  if (!g->loop) {
    free(g);
    return NULL;
  }
  g->context = pw_context_new(pw_thread_loop_get_loop(g->loop),
                              pw_properties_new(PW_KEY_CONFIG_NAME, "client.conf", NULL), 0);
  if (!g->context || pw_thread_loop_start(g->loop) < 0) {
    if (g->context) pw_context_destroy(g->context);
    pw_thread_loop_destroy(g->loop);
    free(g);
    return NULL;
  }
  pw_thread_loop_lock(g->loop);
  g->core = pw_context_connect(
      g->context, pw_properties_new(PW_KEY_APP_NAME, "Audiomap", NULL), 0);
  if (!g->core) {
    ag_fail(g, "Cannot connect to PipeWire.");
    g->broken = true;
    pw_thread_loop_unlock(g->loop);
    return g;
  }
  pw_core_add_listener(g->core, &g->core_listener, &ag_core_events, g);
  g->registry = pw_core_get_registry(g->core, PW_VERSION_REGISTRY, 0);
  pw_registry_add_listener(g->registry, &g->registry_listener, &ag_registry_events, g);
  if (ag_sync(g) < 0 || ag_sync(g) < 0) {
    ag_fail(g, "PipeWire did not answer.");
    g->broken = true;
  }
  pw_thread_loop_unlock(g->loop);
  return g;
}

void ag_close(struct audio_graph *g) {
  if (!g) return;
  pw_thread_loop_lock(g->loop);
  while (g->meters) {
    struct ag_meter *m = g->meters;
    g->meters = m->next;
    ag_meter_destroy(m);
  }
  if (g->metadata) {
    spa_hook_remove(&g->metadata_listener);
    pw_proxy_destroy((struct pw_proxy *)g->metadata);
    g->metadata = NULL;
  }
  ag_targets_clear(g, 0, true);
  while (g->objects) {
    struct ag_object *o = g->objects;
    g->objects = o->next;
    ag_object_free(o);
  }
  if (g->registry) {
    spa_hook_remove(&g->registry_listener);
    pw_proxy_destroy((struct pw_proxy *)g->registry);
  }
  if (g->core) {
    spa_hook_remove(&g->core_listener);
    pw_core_disconnect(g->core);
  }
  pw_thread_loop_unlock(g->loop);
  pw_thread_loop_stop(g->loop);
  pw_context_destroy(g->context);
  pw_thread_loop_destroy(g->loop);
  free(g);
}

/* Live levels on or off: meters exist only while something shows them. */
void ag_set_metering(struct audio_graph *g, int on) {
  if (!g) return;
  pw_thread_loop_lock(g->loop);
  g->metering = on != 0;
  if (!g->broken) ag_meters_reconcile(g);
  pw_thread_loop_unlock(g->loop);
}

/* Periodic pass: 0 ok, -1 connection lost, -2 transient stall. */
int ag_maintain(struct audio_graph *g) {
  if (!g) return -1;
  pw_thread_loop_lock(g->loop);
  int result = ag_sync(g);
  if (result == 0) ag_meters_reconcile(g);
  pw_thread_loop_unlock(g->loop);
  return result;
}

static void ag_hex(char **buf, size_t *left, const char *s) {
  if (!s) s = "";
  for (; *s && *left > 3; s++) {
    int n = snprintf(*buf, *left, "%02x", (unsigned char)*s);
    *buf += n;
    *left -= (size_t)n;
  }
}
static void ag_put(char **buf, size_t *left, const char *fmt, ...) {
  if (*left < 2) return;
  va_list ap;
  va_start(ap, fmt);
  int n = vsnprintf(*buf, *left, fmt, ap);
  va_end(ap);
  if (n < 0) return;
  if ((size_t)n >= *left) n = (int)*left - 1;
  *buf += n;
  *left -= (size_t)n;
}
/* Property from the node, falling back to its client. */
static const char *ag_identity(struct audio_graph *g, struct ag_object *node, const char *key) {
  const char *v = ag_prop(node, key);
  if (v && *v) return v;
  struct ag_object *client = ag_find(g, (uint32_t)ag_num(ag_prop(node, PW_KEY_CLIENT_ID)));
  return client && client->kind == AG_CLIENT ? ag_prop(client, key) : NULL;
}
/* The output a stream plays to now (by its links), or NULL. */
static struct ag_object *ag_playing_on(struct audio_graph *g, struct ag_object *stream) {
  for (struct ag_object *l = g->objects; l; l = l->next) {
    if (l->kind != AG_LINK || l->out_node != stream->id) continue;
    struct ag_object *in = ag_find(g, l->in_node);
    if (ag_is_output(in)) return in;
  }
  return NULL;
}
/* The output a stream is explicitly targeted at (`target.object`), or NULL
 * when it follows the default. */
/* The raw `target.object` of a stream, or NULL. */
static const char *ag_target_raw(struct audio_graph *g, struct ag_object *stream) {
  for (struct ag_target *t = g->targets; t; t = t->next)
    if (t->subject == stream->id) return t->value;
  return NULL;
}
static struct ag_object *ag_targeted_at(struct audio_graph *g, struct ag_object *stream) {
  for (struct ag_target *t = g->targets; t; t = t->next) {
    if (t->subject != stream->id) continue;
    uint64_t serial = ag_num(t->value);
    for (struct ag_object *o = g->objects; o; o = o->next)
      if (o->kind == AG_NODE &&
          ((serial && o->serial == serial) || spa_streq(ag_prop(o, PW_KEY_NODE_NAME), t->value)))
        return o;
    return NULL;
  }
  return NULL;
}
static float ag_level_of(struct audio_graph *g, uint32_t node) {
  int64_t now = ag_millis();
  for (struct ag_meter *m = g->meters; m; m = m->next) {
    if (m->node != node) continue;
    if (now - atomic_load(&m->published_ms) > 250) return 0.f;
    uint32_t bits = atomic_load(&m->peak_bits);
    float peak;
    memcpy(&peak, &bits, 4);
    return isfinite(peak) ? peak : 0.f;
  }
  return -1.f;
}

/* Text snapshot (NUL-terminated), one record per line, tab separated:
 *   graph <healthy> <metadata> <hex error>
 *   default <hex default output> <hex configured default output>  (node.name)
 *   output <node> <serial> <hex node.name> <hex description> <hex nick>
 *          <hex icon name> <hex form factor> <device id> <hex device description>
 *          <volume, -1 unknown> <mute> <level, -1 unmetered> <hex node.group>
 *   stream <node> <serial> <pid> <state> <dont_move> <info> <hex app name>
 *          <hex binary> <hex media name> <hex node.name> <hex icon name>
 *          <hex output it plays on> <hex output it is targeted at>
 *          <volume, -1 unknown> <mute> <level, -1 unmetered>
 * Returns 0, -1 on failure, -2 when the buffer is too small. */
int ag_snapshot(struct audio_graph *g, char *out, size_t size) {
  if (!g || !out || size < 64) return -1;
  pw_thread_loop_lock(g->loop);
  char *buf = out;
  size_t left = size;
  ag_put(&buf, &left, "graph\t%d\t%d\t", !g->broken, g->metadata != NULL);
  ag_hex(&buf, &left, g->error);
  ag_put(&buf, &left, "\ndefault\t");
  ag_hex(&buf, &left, g->default_sink);
  ag_put(&buf, &left, "\t");
  ag_hex(&buf, &left, g->configured_sink);
  ag_put(&buf, &left, "\n");
  for (struct ag_object *o = g->objects; o; o = o->next) {
    if (!ag_is_output(o)) continue;
    const char *name = ag_prop(o, PW_KEY_NODE_NAME);
    if (!name || !*name) continue;
    const char *description = ag_prop(o, PW_KEY_NODE_DESCRIPTION);
    if (!description || !*description) description = ag_prop(o, PW_KEY_NODE_NICK);
    uint64_t device_id = ag_num(ag_prop(o, PW_KEY_DEVICE_ID));
    struct ag_object *device = device_id ? ag_find(g, (uint32_t)device_id) : NULL;
    if (device && device->kind != AG_DEVICE) device = NULL;
    const char *icon = ag_prop(o, PW_KEY_DEVICE_ICON_NAME);
    if ((!icon || !*icon) && device) icon = ag_prop(device, PW_KEY_DEVICE_ICON_NAME);
    const char *form = ag_prop(o, PW_KEY_DEVICE_FORM_FACTOR);
    if ((!form || !*form) && device) form = ag_prop(device, PW_KEY_DEVICE_FORM_FACTOR);
    ag_put(&buf, &left, "output\t%u\t%" PRIu64 "\t", o->id, o->serial);
    ag_hex(&buf, &left, name);
    ag_put(&buf, &left, "\t");
    ag_hex(&buf, &left, description && *description ? description : name);
    ag_put(&buf, &left, "\t");
    ag_hex(&buf, &left, ag_prop(o, PW_KEY_NODE_NICK));
    ag_put(&buf, &left, "\t");
    ag_hex(&buf, &left, icon);
    ag_put(&buf, &left, "\t");
    ag_hex(&buf, &left, form);
    ag_put(&buf, &left, "\t%" PRIu64 "\t", device ? device_id : 0);
    ag_hex(&buf, &left, device ? ag_prop(device, PW_KEY_DEVICE_DESCRIPTION) : NULL);
    ag_put(&buf, &left, "\t%.6f\t%d\t%.6f\t", o->level_known ? (double)o->volume : -1.0,
           o->level_known && o->mute, (double)ag_level_of(g, o->id));
    ag_hex(&buf, &left, ag_prop(o, PW_KEY_NODE_GROUP));
    ag_put(&buf, &left, "\n");
  }
  for (struct ag_object *o = g->objects; o; o = o->next) {
    if (!ag_is_app_stream(o)) continue;
    struct ag_object *on = ag_playing_on(g, o), *at = ag_targeted_at(g, o);
    ag_put(&buf, &left, "stream\t%u\t%" PRIu64 "\t%" PRIu64 "\t%d\t%d\t%d\t", o->id, o->serial,
           ag_num(ag_identity(g, o, PW_KEY_APP_PROCESS_ID)), o->state,
           spa_streq(ag_prop(o, "node.dont-move"), "true"), o->info);
    ag_hex(&buf, &left, ag_identity(g, o, PW_KEY_APP_NAME));
    ag_put(&buf, &left, "\t");
    ag_hex(&buf, &left, ag_identity(g, o, PW_KEY_APP_PROCESS_BINARY));
    ag_put(&buf, &left, "\t");
    ag_hex(&buf, &left, ag_prop(o, PW_KEY_MEDIA_NAME));
    ag_put(&buf, &left, "\t");
    ag_hex(&buf, &left, ag_prop(o, PW_KEY_NODE_NAME));
    ag_put(&buf, &left, "\t");
    ag_hex(&buf, &left, ag_identity(g, o, PW_KEY_APP_ICON_NAME));
    ag_put(&buf, &left, "\t");
    ag_hex(&buf, &left, on ? ag_prop(on, PW_KEY_NODE_NAME) : NULL);
    ag_put(&buf, &left, "\t");
    /* A target that is gone (an unplugged device) still counts: as its raw
     * value, so it can be cleared. */
    ag_hex(&buf, &left, at ? ag_prop(at, PW_KEY_NODE_NAME) : ag_target_raw(g, o));
    ag_put(&buf, &left, "\t%.6f\t%d\t%.6f\n", o->level_known ? (double)o->volume : -1.0,
           o->level_known && o->mute, (double)ag_level_of(g, o->id));
  }
  int result = left < 64 ? -2 : 0;
  pw_thread_loop_unlock(g->loop);
  return result;
}

/* Only the live levels, for redrawing at frame rate: `<node> <level>` lines. */
int ag_levels(struct audio_graph *g, char *out, size_t size) {
  if (!g || !out || size < 16) return -1;
  pw_thread_loop_lock(g->loop);
  char *buf = out;
  size_t left = size;
  out[0] = '\0';
  for (struct ag_meter *m = g->meters; m; m = m->next)
    ag_put(&buf, &left, "%u\t%.6f\n", m->node, (double)ag_level_of(g, m->node));
  int result = left < 16 ? -2 : 0;
  pw_thread_loop_unlock(g->loop);
  return result;
}

static struct ag_object *ag_node_named(struct audio_graph *g, const char *name) {
  for (struct ag_object *o = g->objects; o; o = o->next)
    if (o->kind == AG_NODE && spa_streq(ag_prop(o, PW_KEY_NODE_NAME), name)) return o;
  return NULL;
}

/* Point an application's stream at an output (node.name), or with NULL clear
 * the target so it follows the default output. */
int ag_route(struct audio_graph *g, uint32_t node, uint64_t serial, const char *output) {
  if (!g) return -1;
  pw_thread_loop_lock(g->loop);
  int result = -1;
  struct ag_object *o = ag_find(g, node);
  if (!g->broken && g->metadata && ag_is_app_stream(o) && o->serial == serial &&
      !spa_streq(ag_prop(o, "node.dont-move"), "true")) {
    if (!output) {
      result = pw_metadata_set_property(g->metadata, node, "target.object", NULL, NULL);
    } else {
      struct ag_object *target = ag_node_named(g, output);
      if (ag_is_output(target) && target->serial) {
        char value[32];
        snprintf(value, sizeof(value), "%" PRIu64, target->serial);
        result = pw_metadata_set_property(g->metadata, node, "target.object", "Spa:Id", value);
      }
    }
    if (result >= 0) result = ag_sync(g);
  }
  pw_thread_loop_unlock(g->loop);
  return result < 0 ? -1 : 0;
}

/* Volume (linear 0..1.5, negative keeps it) and mute (negative keeps it) of
 * an application stream or an output, as a desktop mixer sets them. */
int ag_set_level(struct audio_graph *g, uint32_t node, float volume, int mute) {
  if (!g || volume > 1.5f || (volume >= 0.f && !isfinite(volume))) return -1;
  pw_thread_loop_lock(g->loop);
  int result = -1;
  struct ag_object *o = ag_find(g, node);
  if (!g->broken && o && o->proxy && (ag_is_app_stream(o) || ag_is_output(o))) {
    uint8_t buffer[512];
    struct spa_pod_builder pb = SPA_POD_BUILDER_INIT(buffer, sizeof(buffer));
    struct spa_pod_frame frame;
    spa_pod_builder_push_object(&pb, &frame, SPA_TYPE_OBJECT_Props, SPA_PARAM_Props);
    if (volume >= 0.f) {
      uint32_t n = o->channels;
      if (n < 1 || n > SPA_AUDIO_MAX_CHANNELS) n = (uint32_t)ag_num(ag_prop(o, PW_KEY_AUDIO_CHANNELS));
      if (n < 1 || n > SPA_AUDIO_MAX_CHANNELS) n = 2;
      float values[SPA_AUDIO_MAX_CHANNELS];
      for (uint32_t i = 0; i < n; i++) values[i] = volume * volume * volume;
      spa_pod_builder_add(&pb, SPA_PROP_channelVolumes,
                          SPA_POD_Array(sizeof(float), SPA_TYPE_Float, n, values), 0);
    }
    if (mute >= 0) spa_pod_builder_add(&pb, SPA_PROP_mute, SPA_POD_Bool(mute != 0), 0);
    struct spa_pod *props = spa_pod_builder_pop(&pb, &frame);
    if (props && pw_node_set_param((struct pw_node *)o->proxy, SPA_PARAM_Props, 0, props) >= 0)
      result = 0;
  }
  pw_thread_loop_unlock(g->loop);
  return result;
}

/* Make an output (node.name) the desktop's default, as `wpctl set-default`
 * does: the session manager moves every stream that follows the default. */
int ag_set_default_output(struct audio_graph *g, const char *name) {
  if (!g || !name || !*name || strlen(name) >= 200) return -1;
  pw_thread_loop_lock(g->loop);
  int result = -1;
  struct ag_object *target = ag_node_named(g, name);
  if (!g->broken && g->metadata && ag_is_output(target)) {
    char quoted[512], value[600];
    if (spa_json_encode_string(quoted, sizeof(quoted), name) < (int)sizeof(quoted)) {
      snprintf(value, sizeof(value), "{ \"name\": %s }", quoted);
      result = pw_metadata_set_property(g->metadata, PW_ID_CORE, "default.configured.audio.sink",
                                        "Spa:String:JSON", value) < 0
                   ? -1
                   : 0;
    }
  }
  pw_thread_loop_unlock(g->loop);
  return result;
}
