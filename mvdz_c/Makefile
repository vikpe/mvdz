# make test [SANITIZE=1], make fuzz, make test-demos, make mvdz_bench, make format-check
CC ?= cc
CFLAGS ?= -O2
WARNINGS = -std=c99 -Wall -Wextra -Wpedantic -Wconversion -Wshadow -Werror
ifdef SANITIZE
SANITIZERS = -g -fsanitize=address,undefined -fno-sanitize-recover=all
endif
FUZZ_SECS ?= 60

LIBRARY = mvdz.h mvdz.c error.c fields.c math.c streams.c format.c protocol.c numeric.c \
	crc32.c rans.c lz.c segment.c walker.c decode.c
FORMATTED = $(LIBRARY) tests/*.c fuzz/*.c bench/*.c

# The library is one compiler call.
mvdz.o: $(LIBRARY)
	$(CC) $(WARNINGS) $(CFLAGS) -c mvdz.c -o $@

TEST = build/test$(if $(SANITIZE),_sanitize)
$(TEST): $(LIBRARY) tests/test.c tests/sha256.c
	mkdir -p build
	$(CC) $(WARNINGS) $(CFLAGS) $(SANITIZERS) tests/test.c -o $@

.PHONY: test
test: $(TEST)
	./$(TEST) ../vectors

mvdz_bench: $(LIBRARY) bench/mvdz_bench.c
	$(CC) $(WARNINGS) $(CFLAGS) bench/mvdz_bench.c mvdz.c -o $@

# Compresses each demo of $MVDZ_TEST_DATA/sample with the Rust tool, decodes it here
# and compares.
.PHONY: test-demos
test-demos: mvdz_bench
	@test -n "$(MVDZ_TEST_DATA)" || { echo "MVDZ_TEST_DATA is unset"; exit 1; }
	@test -x "$(MVDZ_REFERENCE_CLI)" || { echo "MVDZ_REFERENCE_CLI is not an executable"; exit 1; }
	@mkdir -p build
	@set -e; for demo in "$(MVDZ_TEST_DATA)"/sample/*.mvd; do \
		"$(MVDZ_REFERENCE_CLI)" compress -f "$$demo" build/demo.mvdz; \
		./mvdz_bench decompress build/demo.mvdz build/demo.mvd; \
		cmp "$$demo" build/demo.mvd; \
		echo "ok $$demo"; \
	done

# clang only (libFuzzer); the corpus is seeded from the vectors.
.PHONY: fuzz
fuzz:
	mkdir -p build/corpus
	cp ../vectors/*.mvdz build/corpus
	clang -std=c99 -g -O1 -fsanitize=fuzzer,address,undefined -fno-sanitize-recover=all \
		fuzz/decode.c mvdz.c -o build/fuzz_decode
	./build/fuzz_decode -max_total_time=$(FUZZ_SECS) build/corpus

.PHONY: format format-check
format:
	clang-format -i $(FORMATTED)
format-check:
	clang-format --dry-run -Werror $(FORMATTED)

.PHONY: clean
clean:
	rm -rf build mvdz.o mvdz_bench
