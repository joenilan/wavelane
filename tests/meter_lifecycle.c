#include "../native/audio_graph.c"
#include <assert.h>
int main(void) {
  struct audio_graph *g = ag_open();
  assert(g);
  assert(ag_maintain(g) == 0);
  assert(g->meters == NULL);
  ag_set_metering(g, 1);
  assert(g->metering);
  assert(g->meters != NULL);
  unsigned count = 0;
  for (struct ag_meter *m = g->meters; m; m = m->next) count++;
  ag_set_metering(g, 0);
  assert(!g->metering);
  assert(g->meters == NULL);
  assert(ag_maintain(g) == 0);
  assert(g->meters == NULL);
  ag_close(g);
  printf("PASS: %u private output meter(s) created, destroyed and not recreated\n", count);
}
