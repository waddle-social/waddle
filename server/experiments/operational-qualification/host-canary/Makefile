# STATIC_CC must explicitly select the reviewed Linux musl compiler.
ifndef STATIC_CC
$(error Set STATIC_CC to the absolute path of the pinned musl gcc)
endif

CFLAGS := -std=c11 -O2 -Wall -Wextra -Werror -Wpedantic -Wconversion -Wshadow \
          -Wformat=2 -Wstrict-prototypes -Wmissing-prototypes \
          -fstack-protector-strong -fPIE -ffile-prefix-map=$(CURDIR)=.
LDFLAGS := -static-pie -Wl,-z,relro,-z,now,-z,noexecstack

.PHONY: all check check-static check-analysis
all: build/probe build/probe-controlled

build:
	mkdir -p build

build/probe: main.c inspect.c parse.c frame.c probe.h | build
	$(STATIC_CC) $(CFLAGS) main.c inspect.c parse.c frame.c $(LDFLAGS) -o $@

build/probe-controlled: main.c inspect.c parse.c frame.c barrier.c barrier_io.c probe.h barrier.h | build
	$(STATIC_CC) $(CFLAGS) -DPROBE_CONTROLLED main.c inspect.c parse.c frame.c barrier.c barrier_io.c $(LDFLAGS) -o $@

# OS adapters and probe main are absent from both fixture binaries.
build/fixture-test: fixture_test.c parse.c frame.c probe.h | build
	$(STATIC_CC) $(CFLAGS) fixture_test.c parse.c frame.c $(LDFLAGS) -o $@

build/barrier-fixture-test: barrier_fixture_test.c barrier.c parse.c probe.h barrier.h | build
	$(STATIC_CC) $(CFLAGS) barrier_fixture_test.c barrier.c parse.c $(LDFLAGS) -o $@

check: build/fixture-test build/barrier-fixture-test
	./build/fixture-test > build/synthetic-frame
	./build/barrier-fixture-test
	python3 -B -m unittest -v test_frame.py
	python3 -B check_frame.py < build/synthetic-frame

# Inspect both built ELFs as data. This target never executes either probe.
check-static: build/probe build/probe-controlled
	sh check-static.sh

build/analyze-%.o: %.c probe.h barrier.h | build
	$(STATIC_CC) $(CFLAGS) -fanalyzer -c $< -o $@

build/analyze-main-controlled.o: main.c probe.h barrier.h | build
	$(STATIC_CC) $(CFLAGS) -DPROBE_CONTROLLED -fanalyzer -c $< -o $@

check-analysis: build/analyze-main.o build/analyze-inspect.o build/analyze-parse.o \
                build/analyze-frame.o build/analyze-fixture_test.o \
                build/analyze-main-controlled.o build/analyze-barrier.o \
                build/analyze-barrier_io.o build/analyze-barrier_fixture_test.o
