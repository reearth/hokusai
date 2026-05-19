CFLAGS  ?= -O2 -Wall -Wextra
CFLAGS  += $(shell pkg-config --cflags libmypaint json-c)
LDLIBS  := $(shell pkg-config --libs   libmypaint json-c) -lm

BIN := libmypaint-render

$(BIN): render.c
	$(CC) $(CFLAGS) -o $@ render.c $(LDLIBS)

clean:
	rm -f $(BIN)

.PHONY: clean
