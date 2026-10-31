ifndef STATIC_CC
$(error Set STATIC_CC to an absolute pinned Linux musl gcc path)
endif

CFLAGS := -std=c11 -O2 -Wall -Wextra -Werror -Wpedantic -Wconversion -Wshadow \
          -Wformat=2 -Wstrict-prototypes -Wmissing-prototypes -fstack-protector-strong \
          -fPIE -ffile-prefix-map=$(CURDIR)=.
LDFLAGS := -static-pie -Wl,-z,relro,-z,now,-z,noexecstack

.PHONY: all check check-analysis check-static
all: build/bootstrap build/witness

build/bootstrap: bootstrap.c build/paths.h
	$(STATIC_CC) $(CFLAGS) -Ibuild $< $(LDFLAGS) -o $@

build/witness: witness.c filter.c filter.h
	mkdir -p build
	$(STATIC_CC) $(CFLAGS) witness.c filter.c $(LDFLAGS) -o $@

build/filter-test: filter_test.c filter.c filter.h
	mkdir -p build
	$(STATIC_CC) $(CFLAGS) filter_test.c filter.c $(LDFLAGS) -o $@

check: build/filter-test
	./build/filter-test
	python3 -B -m unittest -v test_policy.py test_lifecycle.py

check-analysis: build/paths.h
	$(STATIC_CC) $(CFLAGS) -Ibuild -fanalyzer -c bootstrap.c -o build/analyze-bootstrap.o
	$(STATIC_CC) $(CFLAGS) -fanalyzer -c witness.c -o build/analyze-witness.o
	$(STATIC_CC) $(CFLAGS) -fanalyzer -c filter.c -o build/analyze-filter.o

check-static: all
	sh check-static.sh
