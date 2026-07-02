# keystroke-harness — build + self-test (spikes 4b/5)
#
#   make        build the injector (keyinject)
#   make test   build + injector arg-validation + reducer self-test
#   make clean
#
# The injector never posts anything at build/test time: `--help` exits before
# the Accessibility check, and test-reduce.py only exercises the Python
# reducer against a synthetic xctrace export.

SWIFTC ?= xcrun swiftc

all: keyinject

keyinject: keyinject.swift
	$(SWIFTC) -O -o $@ $<

test: keyinject
	./keyinject --help > /dev/null
	@./keyinject 2>/dev/null; test $$? -eq 64 && echo "PASS usage exit code (64)"
	python3 test-reduce.py

clean:
	rm -f keyinject

.PHONY: all test clean
