// src/backend/c/runtime.h
/**
 * @file runtime.h
 * @brief Header for the Jophet runtime library.
 *
 * This file defines the public interface for the Jophet runtime. It includes
 * the type definitions for built-in types like `JophetString` and `JophetVector`,
 * as well as the function prototypes for creating, manipulating, and deleting
 * instances of these types. This header is included by the C code generated
 * by the Jophet compiler.
 */

#ifndef JOPHET_RUNTIME_H
#define JOPHET_RUNTIME_H

#include <stddef.h>
#include <stdbool.h>
#include <stdint.h>

// --- Utility Functions ---

// Forward-declare the JophetString struct so its pointer can be used in prototypes.
typedef struct JophetString JophetString;

/**
 * @brief Allocates a block of memory of the given size.
 *
 * This function is an unsafe wrapper around `malloc`. It is intended to be called
 * only from within an `allow` block in Jophet.
 *
 * @param size The number of bytes to allocate.
 * @return A void pointer to the allocated memory, or NULL if allocation fails.
 */
void* jophet_allocate(size_t size);

/**
 * @brief Deallocates a block of memory.
 *
 * This function is an unsafe wrapper around `free`. It is intended to be called
 * only from within an `allow` block in Jophet.
 *
 * @param ptr A void pointer to the memory to deallocate.
 */
void jophet_deallocate(void* ptr);

/**
 * @brief Performs a runtime bounds check for array and vector access.
 *
 * This function checks if the given `index` is within the valid range [0, length).
 * If the index is out of bounds, it prints a fatal error message to stderr and
 * terminates the program with a non-zero exit code. If the index is valid, it
 * returns the index, allowing it to be used directly in an access expression.
 *
 * @param index The index being accessed.
 * @param length The total number of elements in the collection.
 * @param file The name of the source file where the access occurred.
 * @param line The line number in the source file where the access occurred.
 * @return The `index` if it is valid.
 * @note This function will cause program termination on failure.
 */
size_t jophet_bounds_check(size_t index, size_t length, const char* file, int line);

/**
 * @brief Handles a panic when a `try` expression fails in a non-fallible context.
 *
 * This function is called by code generated for a `try` expression that is not
 * inside a function returning a `Fallible` type. It prints a formatted fatal error
 * message, including the specific error value that caused the panic, and terminates
 * the program with a non-zero exit code.
 *
 * @param err_ptr A void pointer to the error data.
 * @param print_fn A function pointer to the appropriate `_print` function for the error's type.
 * @param file The name of the source file where the `try` expression occurred.
 * @param line The line number in the source file where the `try` expression occurred.
 * @note This function will cause program termination.
 */
void jophet_panic_on_err(const void* err_ptr, void (*print_fn)(const void*), const char* file, int line);

/**
 * @brief Handles a panic with a simple string message.
 *
 * This function is a convenience for runtime errors that don't have a structured
 * error type, such as calling `minimum` on an empty collection.
 *
 * @param message The error message to display.
 * @param file The name of the source file where the error occurred.
 * @param line The line number in the source file where the error occurred.
 * @note This function will cause program termination.
 */
void jophet_panic_on_err_message(const char* message, const char* file, int line);

/**
 * @brief Handles a panic when a Python FFI call fails.
 *
 * This function prints a formatted fatal error message including the Python exception
 * details and source location of the FFI call, then terminates the program.
 *
 * @param py_err_msg A pointer to a JophetString containing the Python exception message.
 * @param file The name of the source file where the FFI call occurred.
 * @param line The line number in the source file where the FFI call occurred.
 * @note This function will cause program termination.
 */
void jophet_panic_on_py_err(const JophetString* py_err_msg, const char* file, int line);


// --- Built-in String Type ---

/**
 * @struct JophetString
 * @brief A dynamic, heap-allocated string type.
 *
 * This struct represents a string in Jophet. It is similar to a C++ std::string
 * or a Rust String, managing its own memory.
 *
 * @var JophetString::data
 * A pointer to the heap-allocated character buffer. The buffer is always
 * null-terminated to allow for easy interoperability with C functions.
 * @var JophetString::len
 * The number of characters in the string (its length).
 * @var JophetString::capacity
 * The total number of bytes allocated in the data buffer (excluding the
 * null terminator).
 */
typedef struct JophetString {
    char* data;
    size_t len;
    size_t capacity;
} JophetString;

/**
 * @struct JophetVector
 * @brief A dynamic, type-agnostic vector (dynamic array).
 *
 * This struct represents a generic vector in Jophet. It manages a contiguous
 * block of memory to store elements of a uniform size. Multi-dimensional vectors
 * are created by nesting vectors (e.g., a vector where the elements are themselves vectors).
 *
 * @var JophetVector::data
 * A void pointer to the heap-allocated buffer for the vector elements.
 * @var JophetVector::len
 * The number of elements currently stored in the vector.
 * @var JophetVector::capacity
 * The total number of elements that can be stored in the currently allocated buffer.
 * @var JophetVector::elem_size
 * The size in bytes of a single element in the vector. This is set at creation time.
 */
typedef struct {
    void* data;
    size_t len;
    size_t capacity;
    size_t elem_size;
} JophetVector;

// Type definitions for function pointers used by the dictionary for deep operations.
typedef void (*Jophet_Item_Delete_Fn)(void*);
typedef void* (*Jophet_Item_Clone_Fn)(const void*);
typedef void (*Jophet_Item_Print_Fn)(const void*);
typedef bool (*Jophet_Comparison_Fn)(const void*, const void*);
typedef size_t (*Jophet_Inner_Len_Fn)(const void*);
typedef const void* (*Jophet_Inner_Data_Fn)(const void*);


/**
 * @struct JophetDictionaryEntry
 * @brief An entry (key-value pair) in a dictionary's linked list for handling hash collisions.
 */
typedef struct JophetDictionaryEntry {
    void* key;
    void* value;
    struct JophetDictionaryEntry* next;
} JophetDictionaryEntry;

/**
 * @struct JophetDictionary
 * @brief A dynamic, type-agnostic dictionary (hash map).
 *
 * This struct represents a generic dictionary in Jophet. It uses a separate-chaining
 * hash table to store key-value pairs of a uniform size.
 *
 * @var JophetDictionary::buckets
 * An array of pointers to dictionary entries, representing the hash buckets.
 * @var JophetDictionary::capacity
 * The number of buckets in the hash table.
 * @var JophetDictionary::len
 * The number of key-value pairs currently stored in the dictionary.
 * @var JophetDictionary::key_size
 * The size in bytes of a single key.
 * @var JophetDictionary::value_size
 * The size in bytes of a single value.
 * @var JophetDictionary::key_delete_fn
 * A function pointer to the destructor for keys, for deep deletion. Can be NULL.
 * @var JophetDictionary::value_delete_fn
 * A function pointer to the destructor for values, for deep deletion. Can be NULL.
 * @var JophetDictionary::key_clone_fn
 * A function pointer to the cloner for keys, for deep cloning. Can be NULL.
 * @var JophetDictionary::value_clone_fn
 * A function pointer to the cloner for values, for deep cloning. Can be NULL.
 */
typedef struct {
    JophetDictionaryEntry** buckets;
    size_t capacity;
    size_t len;
    size_t key_size;
    size_t value_size;
    Jophet_Item_Delete_Fn key_delete_fn;
    Jophet_Item_Delete_Fn value_delete_fn;
    Jophet_Item_Clone_Fn key_clone_fn;
    Jophet_Item_Clone_Fn value_clone_fn;
} JophetDictionary;


/**
 * @struct jophet_empty_env
 * @brief A placeholder struct for the environment of closures that capture no variables.
 * C requires structs to have at least one member.
 */
typedef struct {
    uint8_t _dummy;
} jophet_empty_env;

/**
 * @struct JophetClosure
 * @brief A generic representation of a closure.
 *
 * This struct represents a first-class function that can capture variables
 * from its environment. It consists of a function pointer to the closure's
 * code, a void pointer to a struct containing the captured variables, and a
 * pointer to a function that can destroy the environment.
 *
 * @var JophetClosure::fn_ptr
 * A generic function pointer to the underlying C function for the closure.
 * This pointer must be cast to the correct function signature before being called.
 * @var JophetClosure::env
 * A void pointer to the heap-allocated environment struct that holds the
 * values of the captured variables.
 * @var JophetClosure::delete_env_fn
 * A function pointer to the destructor for the environment. This function is
 * responsible for cleaning up any owned data within the environment before
 * freeing the environment struct itself. It can be NULL if there are no captures.
 * @var JophetClosure::clone_env_fn
 * A function pointer to a function that can perform a deep copy of the
 * environment. This is essential for cloning closures that own data.
 */
typedef struct {
    void (*fn_ptr)(void);
    void* env;
    void (*delete_env_fn)(void*);
    void* (*clone_env_fn)(const void*);
} JophetClosure;


/**
 * @brief Creates a new, empty JophetString.
 * @return An initialized, empty JophetString.
 */
JophetString String_new(void);

/**
 * @brief Creates a new JophetString from a null-terminated C string.
 * @param c_str The null-terminated C string to copy.
 * @return A new JophetString containing a copy of c_str.
 */
JophetString String_new_from(const char* c_str);

/**
 * @brief Creates a deep copy of a JophetString.
 * @param s A pointer to the JophetString to clone.
 * @return A new JophetString with a separate copy of the data.
 */
JophetString String_clone(const JophetString* s);

/**
 * @brief Deallocates the memory used by a JophetString.
 * @param s A pointer to the JophetString to delete.
 */
void String_delete(JophetString* s);

/**
 * @brief Gets the length of a JophetString.
 * @param s A pointer to the JophetString.
 * @return The length of the string in bytes.
 */
size_t String_length(JophetString* s);

// --- Result Type for String.get() and other optional char methods ---
typedef struct { bool is_ok; union { char ok; int _dummy_err; } data; } Result_Char_Nothing;

/**
 * @brief Gets the character at a specific byte index in a JophetString.
 * @param s A pointer to the JophetString.
 * @param index The byte index of the character to retrieve.
 * @return A Result struct. If the index is in bounds, `is_ok` is true and `data.ok`
 *         contains the character. Otherwise, `is_ok` is false.
 */
Result_Char_Nothing String_get(const JophetString* s, uint64_t index);

/**
 * @brief Splits a string into a vector of its characters.
 * @param s A pointer to the source JophetString. The string is not modified.
 * @return A new JophetVector containing each character from the source string.
 */
JophetVector String_characters(const JophetString* s);

/**
 * @brief Checks if a string is empty.
 * @param s A pointer to the JophetString.
 * @return `true` if the string length is 0, `false` otherwise.
 */
bool String_isEmpty(const JophetString* s);

/**
 * @brief Removes and returns the last character of a string.
 * @param s A pointer to the JophetString to modify.
 * @return A `Result_Char_Nothing`. If the string was not empty, `is_ok` is true and `data.ok`
 *         contains the removed character. Otherwise, `is_ok` is false.
 */
Result_Char_Nothing String_pop(JophetString* s);

/**
 * @brief Returns the first character of a string without removing it.
 * @param s A pointer to the JophetString.
 * @return A `Result_Char_Nothing`. If the string is not empty, `is_ok` is true and `data.ok`
 *         contains the first character. Otherwise, `is_ok` is false.
 */
Result_Char_Nothing String_first(const JophetString* s);

/**
 * @brief Returns the last character of a string without removing it.
 * @param s A pointer to the JophetString.
 * @return A `Result_Char_Nothing`. If the string is not empty, `is_ok` is true and `data.ok`
 *         contains the last character. Otherwise, `is_ok` is false.
 */
Result_Char_Nothing String_last(const JophetString* s);

/**
 * @brief Checks if a string contains a given substring.
 * @param s A pointer to the JophetString to search within.
 * @param substring A pointer to the JophetString to search for.
 * @return `true` if the substring is found, `false` otherwise.
 */
bool String_contains(const JophetString* s, const JophetString* substring);


// --- Char Utilities ---

/**
 * @brief Checks if a character is alphanumeric.
 * @param c The character to check.
 * @return `true` if the character is a letter or a digit, `false` otherwise.
 */
bool jophet_char_is_alphanumeric(char c);

/**
 * @brief Checks if a character is alphabetic.
 * @param c The character to check.
 * @return `true` if the character is a letter, `false` otherwise.
 */
bool jophet_char_is_alphabetic(char c);

/**
 * @brief Checks if a character is a digit (0-9).
 * @param c The character to check.
 * @return `true` if the character is a digit, `false` otherwise.
 */
bool jophet_char_is_digit(char c);

/**
 * @brief Checks if a character is whitespace.
 * @param c The character to check.
 * @return `true` if the character is a space, tab, newline, etc., `false` otherwise.
 */
bool jophet_char_is_whitespace(char c);

// --- String Formatting/Building ---

/**
 * @brief Appends a null-terminated C string to a JophetString.
 * @param builder A pointer to the JophetString being built.
 * @param c_str The C string to append.
 */
void String_builder_append(JophetString* builder, const char* c_str);

/**
 * @brief Appends another JophetString to a JophetString.
 * @param builder A pointer to the destination JophetString.
 * @param s A pointer to the source JophetString to append.
 */
void String_builder_append_string(JophetString* builder, JophetString* s);

/**
 * @brief Appends a 64-bit integer to a JophetString.
 * @param builder A pointer to the JophetString.
 * @param val The int64_t value to append.
 */
void String_builder_append_int64(JophetString* builder, int64_t val);

/**
 * @brief Appends a double-precision float to a JophetString.
 * @param builder A pointer to the JophetString.
 * @param val The double value to append.
 */
void String_builder_append_float64(JophetString* builder, double val);

/**
 * @brief Appends a single character to a JophetString.
 * @param builder A pointer to the JophetString.
 * @param val The char value to append.
 */
void String_builder_append_char(JophetString* builder, char val);

/**
 * @brief Appends a boolean value ("true" or "false") to a JophetString.
 * @param builder A pointer to the JophetString.
 * @param val The boolean value to append.
 */
void String_builder_append_bool(JophetString* builder, bool val);

/**
 * @brief A generic function to "sprint" (string print) any printable type into a builder.
 * This is a generic helper that requires a function pointer to the actual sprint implementation.
 * @param builder The string builder to append to.
 * @param data A void pointer to the data to be printed.
 * @param sprint_fn A function pointer of type `void (*)(JophetString*, const void*)`.
 */
void jophet_sprint(JophetString* builder, const void* data, void (*sprint_fn)(JophetString*, const void*));

// --- User Input ---

/**
 * @brief Reads a line of input from stdin.
 * 
 * Optionally prints a prompt to the user. Reads characters until a newline
 * or EOF is encountered. The returned string does not include the newline.
 * 
 * @param prompt A null-terminated C string to display as a prompt. Can be NULL.
 * @return A new JophetString containing the user's input.
 */
JophetString input(const char* prompt);


// --- Built-in Vector Type ---

/**
 * @brief Creates a new, empty JophetVector for a given element size.
 * @param elem_size The size in bytes of each element.
 * @return An initialized, empty JophetVector.
 */
JophetVector Vector_new(size_t elem_size);

/**
 * @brief Creates a new JophetVector by copying data from a C array.
 * @param elem_size The size in bytes of each element.
 * @param source_data A const void pointer to the beginning of the source C array. The data is not modified.
 * @param source_len The number of elements in the source array.
 * @return A new JophetVector containing a copy of the array data.
 */
JophetVector Vector_new_from_array(size_t elem_size, const void* source_data, size_t source_len);

/**
 * @brief Creates a deep copy of a JophetVector.
 * @param v A pointer to the JophetVector to clone.
 * @return A new JophetVector with a separate copy of the data. Note: This is a shallow
 *         copy of the elements themselves. If the elements are pointers, the pointers
 *         are copied, not the data they point to.
 */
JophetVector Vector_clone(const JophetVector* v);

/**
 * @brief Deallocates the memory used by a JophetVector.
 * @param v A pointer to the JophetVector to delete.
 */
void Vector_delete(JophetVector* v);

/**
 * @brief Pushes a new item onto the end of a JophetVector.
 * @param v A pointer to the JophetVector.
 * @param item A void pointer to the item to be pushed.
 */
void Vector_push(JophetVector* v, void* item);

/**
 * @brief Gets the number of elements in a JophetVector.
 * @param v A pointer to the JophetVector.
 * @return The number of elements in the vector.
 */
size_t Vector_length(JophetVector* v);

// A generic Result type for dictionary lookups.
// `ok` will be a pointer to the value, which must be cast by the caller.
// `err` is a placeholder for `Nothing`.
typedef struct { bool is_ok; union { void* ok; int _dummy_err; } data; } Result_void_ptr_void;
typedef struct { bool is_ok; union { void* ok; int _dummy_err; } data; } Result_void_ptr_Nothing;

/**
 * @brief Checks if a vector is empty.
 * @param v A pointer to the JophetVector.
 * @return `true` if the vector length is 0, `false` otherwise.
 */
bool Vector_isEmpty(const JophetVector* v);

/**
 * @brief The internal implementation for popping an element. This is called by a
 *        type-safe wrapper generated by the compiler. It copies the popped element
 *        into a destination buffer.
 * @param v A pointer to the JophetVector to modify.
 * @param dest A void pointer to a buffer where the popped element will be copied.
 * @return `true` if an element was popped and copied, `false` if the vector was empty.
 */
bool Vector_pop_impl(JophetVector* v, void* dest);

/**
 * @brief The internal implementation for peeking at the first element. This function
 *        safely copies the first element's data into a destination buffer.
 * @param v A pointer to the JophetVector.
 * @param dest A void pointer to a buffer where the first element will be copied.
 * @return `true` if an element was peeked and copied, `false` if the vector was empty.
 */
bool Vector_first_impl(const JophetVector* v, void* dest);

/**
 * @brief The internal implementation for peeking at the last element. This function
 *        safely copies the last element's data into a destination buffer.
 * @param v A pointer to the JophetVector.
 * @param dest A void pointer to a buffer where the last element will be copied.
 * @return `true` if an element was peeked and copied, `false` if the vector was empty.
 */
bool Vector_last_impl(const JophetVector* v, void* dest);

/**
 * @brief Checks if a vector contains a given element.
 * @param v A pointer to the JophetVector to search within.
 * @param item A pointer to the item to search for. The comparison is done via `memcmp`.
 * @return `true` if the item is found, `false` otherwise.
 */
bool Vector_contains(const JophetVector* v, const void* item);

/**
 * @brief Creates a new JophetVector by performing a shallow copy of a slice of data.
 * @param data_ptr A pointer to the start of the source data.
 * @param original_len The total length of the source data.
 * @param elem_size The size of each element.
 * @param start The starting index of the slice (inclusive).
 * @param end The ending index of the slice (exclusive).
 * @param file Source file for error reporting.
 * @param line Source line for error reporting.
 * @return A new JophetVector containing the sliced data.
 */
JophetVector jophet_slice_shallow(const void* data_ptr, size_t original_len, size_t elem_size, size_t start, size_t end, const char* file, int line);

/**
 * @brief Creates a new JophetVector by performing a deep copy of a slice of data.
 * This is for elements that are owned types (e.g., a vector of strings).
 * The function now correctly manages the memory of temporary cloned items internally.
 * @param data_ptr A pointer to the start of the source data.
 * @param original_len The total length of the source data.
 * @param elem_size The size of each element.
 * @param start The starting index of the slice (inclusive).
 * @param end The ending index of the slice (exclusive).
 * @param clone_fn A function pointer to a thunk that can clone an individual element.
 * @param file Source file for error reporting.
 * @param line Source line for error reporting.
 * @return A new JophetVector containing cloned copies of the sliced elements.
 */
JophetVector jophet_slice_deep(const void* data_ptr, size_t original_len, size_t elem_size, size_t start, size_t end, Jophet_Item_Clone_Fn clone_fn, const char* file, int line);

/**
 * @brief Creates a new JophetString from a slice of a string or character array.
 * @param data_ptr A pointer to the start of the source character data.
 * @param original_len The total length of the source data.
 * @param elem_size The size of a character (always 1).
 * @param start The starting index of the slice (inclusive).
 * @param end The ending index of the slice (exclusive).
 * @param clone_fn Unused for strings, can be NULL.
 * @param file Source file for error reporting.
 * @param line Source line for error reporting.
 * @return A new JophetString containing the sliced characters.
 */
JophetString jophet_string_slice(const void* data_ptr, size_t original_len, size_t elem_size, size_t start, size_t end, Jophet_Item_Clone_Fn clone_fn, const char* file, int line);

/**
 * @brief Flattens a nested collection (Vector<Vector<T>> or Array<Array<T>>) into a single Vector<T>.
 * 
 * This is the internal, unsafe implementation. It takes void pointers and relies on the
 * generated type-safe wrapper to be called correctly. It performs a deep copy of elements if a
 * clone function is provided, otherwise it does a shallow copy.
 * 
 * @param collection A pointer to the outer collection (either a JophetVector or a C array).
 * @param outer_len The number of elements in the outer collection.
 * @param inner_len_fn A function pointer that can retrieve the length of an inner collection.
 * @param elem_size The size in bytes of the innermost element type.
 * @param inner_data_fn A function pointer that can retrieve a pointer to the data of an inner collection.
 * @param clone_fn A function pointer to a function that can deep-clone an element. Can be NULL for shallow copies.
 * @return A new flattened `JophetVector`. Program will terminate on memory allocation failure.
 */
JophetVector jophet_flatten_impl(const void* collection, size_t outer_len, Jophet_Inner_Len_Fn inner_len_fn, size_t elem_size, Jophet_Inner_Data_Fn inner_data_fn, Jophet_Item_Clone_Fn clone_fn);


// --- Built-in Dictionary Type ---

/**
 * @brief Creates a new, empty JophetDictionary for given key and value sizes and their handlers.
 * @param key_size The size in bytes of each key.
 * @param value_size The size in bytes of each value.
 * @param key_delete_fn A function pointer to the key destructor. Can be NULL.
 * @param value_delete_fn A function pointer to the value destructor. Can be NULL.
 * @param key_clone_fn A function pointer to the key cloner. Can be NULL.
 * @param value_clone_fn A function pointer to the value cloner. Can be NULL.
 * @return An initialized, empty JophetDictionary.
 */
JophetDictionary Dictionary_new(size_t key_size, size_t value_size,
                                Jophet_Item_Delete_Fn key_delete_fn, Jophet_Item_Delete_Fn value_delete_fn,
                                Jophet_Item_Clone_Fn key_clone_fn, Jophet_Item_Clone_Fn value_clone_fn);

/**
 * @brief Deallocates all memory used by a JophetDictionary, performing a deep delete
 *        on keys and values if destructor functions were provided.
 * @param dict A pointer to the JophetDictionary to delete.
 */
void Dictionary_delete(JophetDictionary* dict);

/**
 * @brief Creates a deep clone of a JophetDictionary.
 * @param dict A pointer to the dictionary to clone.
 * @return A new JophetDictionary with deep copies of all keys and values.
 */
JophetDictionary Dictionary_clone(const JophetDictionary* dict);

/**
 * @brief Sets a key-value pair in a JophetDictionary.
 * If the key already exists, its value is updated. Otherwise, a new entry is created.
 * @param dict A pointer to the JophetDictionary.
 * @param key A pointer to the key. The data is copied.
 * @param value A pointer to the value. The data is copied.
 */
void Dictionary_set(JophetDictionary* dict, const void* key, const void* value);

/**
 * @brief Prints a representation of the dictionary to stdout.
 * @param dict A pointer to the dictionary to print.
 * @param key_print_fn A function pointer to a function that can print a key.
 * @param value_print_fn A function pointer to a function that can print a value.
 */
void Dictionary_print(const JophetDictionary* dict, Jophet_Item_Print_Fn key_print_fn, Jophet_Item_Print_Fn value_print_fn);

/**
 * @brief Gets the value associated with a key from a JophetDictionary.
 * @param dict A pointer to the JophetDictionary.
 * @param key A pointer to the key to look up.
 * @return A Result struct. If the key is found, `is_ok` is true and `data.ok` is
 *         a pointer to the value. If not found, `is_ok` is false.
 */
Result_void_ptr_void Dictionary_get(const JophetDictionary* dict, const void* key);


// --- Built-in Closure Type ---

/**
 * @brief Deallocates a JophetClosure and its captured environment.
 *
 * If the closure has a non-NULL environment and a `delete_env_fn` function
 * pointer, this function will call the destructor to perform a deep delete of
 * the captured data before freeing the environment struct itself.
 *
 * @param c A pointer to the JophetClosure to delete.
 */
void JophetClosure_delete(JophetClosure* c);

/**
 * @brief Creates a deep copy of a JophetClosure.
 *
 * This function creates a new JophetClosure struct and performs a deep copy
 * of its environment if a `clone_env_fn` is provided. This is crucial for
 * safely copying closures that own data.
 *
 * @param c A pointer to the JophetClosure to clone.
 * @return A new JophetClosure with its own copy of the environment.
 */
JophetClosure JophetClosure_clone(const JophetClosure* c);


// --- Built-in Error Types ---

/**
 * @struct FfiError
 * @brief An error that occurs during a Foreign Function Interface (FFI) call.
 * This is primarily used for the Python FFI.
 * @var FfiError::tag The variant of the FFI error.
 * @var FfiError::data A union containing an optional message payload.
 */
typedef enum {
    FfiError_ModuleNotFound,
    FfiError_AttributeNotFound,
    FfiError_ConversionFailed,
    FfiError_PythonException,
} FfiError_Tag;
typedef struct { FfiError_Tag tag; union { JophetString Message; } data; } FfiError;


/**
 * @struct Error
 * @brief A general-purpose error containing a string message.
 */
typedef enum {
    Error_Message,
} Error_Tag;
typedef struct { Error_Tag tag; union { JophetString Message; } data; } Error;

/**
 * @struct ParseError
 * @brief An error that occurs during string parsing via the `parse` function.
 * @var ParseError::tag The variant of the parse error.
 */
typedef enum {
    ParseError_InvalidFormat,
    ParseError_OutOfRange,
} ParseError_Tag;
typedef struct { ParseError_Tag tag; } ParseError;

/**
 * @struct IoError
 * @brief An error that occurs during a file I/O operation.
 * @var IoError::tag The variant of the I/O error.
 * @var IoError::data A union containing an optional message payload.
 */
typedef enum {
    IoError_NotFound,
    IoError_AccessDenied,
    IoError_ReadFailed,
    IoError_WriteFailed,
    IoError_Other,
} IoError_Tag;
typedef struct { IoError_Tag tag; union { JophetString Other; } data; } IoError;

/**
 * @struct CommandError
 * @brief An error that occurs while executing a system command.
 * @var CommandError::tag The variant of the command error.
 * @var CommandError::data A union containing optional payloads.
 */
typedef enum {
    CommandError_Failed,
    CommandError_TerminatedAbnormally,
    CommandError_NotFound,
} CommandError_Tag;
typedef struct { CommandError_Tag tag; union { int32_t Failed; } data; } CommandError;

// --- Built-in Error Type Print Functions ---
void FfiError_print(const FfiError* s);
void Error_print(const Error* s);
void ParseError_print(const ParseError* s);
void IoError_print(const IoError* s);
void CommandError_print(const CommandError* s);


// --- String Parsing ---
// The Result structs are now updated to use the structured ParseError type.
typedef struct { bool is_ok; union { int8_t ok; ParseError err; } data; } Result_int8_t_ParseError;
typedef struct { bool is_ok; union { int16_t ok; ParseError err; } data; } Result_int16_t_ParseError;
typedef struct { bool is_ok; union { int32_t ok; ParseError err; } data; } Result_int32_t_ParseError;
typedef struct { bool is_ok; union { int64_t ok; ParseError err; } data; } Result_int64_t_ParseError;
typedef struct { bool is_ok; union { uint8_t ok; ParseError err; } data; } Result_uint8_t_ParseError;
typedef struct { bool is_ok; union { uint16_t ok; ParseError err; } data; } Result_uint16_t_ParseError;
typedef struct { bool is_ok; union { uint32_t ok; ParseError err; } data; } Result_uint32_t_ParseError;
typedef struct { bool is_ok; union { uint64_t ok; ParseError err; } data; } Result_uint64_t_ParseError;
typedef struct { bool is_ok; union { float ok; ParseError err; } data; } Result_float_ParseError;
typedef struct { bool is_ok; union { double ok; ParseError err; } data; } Result_double_ParseError;

Result_int8_t_ParseError parse_int8(const JophetString* s);
Result_int16_t_ParseError parse_int16(const JophetString* s);
Result_int32_t_ParseError parse_int32(const JophetString* s);
Result_int64_t_ParseError parse_int64(const JophetString* s);
Result_uint8_t_ParseError parse_uint8(const JophetString* s);
Result_uint16_t_ParseError parse_uint16(const JophetString* s);
Result_uint32_t_ParseError parse_uint32(const JophetString* s);
Result_uint64_t_ParseError parse_uint64(const JophetString* s);
Result_float_ParseError parse_float32(const JophetString* s);
Result_double_ParseError parse_float64(const JophetString* s);


// --- System Commands ---

/**
 * @brief Executes one or more shell commands in sequence.
 * 
 * Takes a count of commands followed by a variable number of pointers to JophetString.
 * It executes each command sequentially. If any command fails, it stops and returns
 * an error result containing a structured CommandError. If all commands succeed, it returns
 * an OK result containing the exit code of the last command.
 * 
 * @param num_commands The number of command string arguments that follow.
 * @param ... Variable arguments, each being a `const JophetString*`.
 * @return A Result_int32_t_CommandError indicating success or failure.
 */
typedef struct { bool is_ok; union { int32_t ok; CommandError err; } data; } Result_int32_t_CommandError;
Result_int32_t_CommandError jophet_command(int num_commands, ...);


// --- File I/O ---
// The Result types are updated to use the structured IoError type.
typedef struct { bool is_ok; union { JophetString ok; IoError err; } data; } Result_JophetString_IoError;
typedef struct { bool is_ok; union { JophetVector ok; IoError err; } data; } Result_JophetVector_IoError;
typedef struct { bool is_ok; union { int _dummy; IoError err; } data; } Result_void_IoError;


Result_JophetString_IoError jophet_read(const JophetString* path);
Result_JophetVector_IoError jophet_read_lines(const JophetString* path);
Result_void_IoError jophet_write(const JophetString* path, const JophetString* content);
Result_void_IoError jophet_write_lines(const JophetString* path, const JophetVector* lines);


// --- Math ---

/**
 * @brief Computes integer exponentiation with runtime safety checks.
 *
 * Calculates `base` to the power of `exp`. This function uses a simple
 * iterative approach (exponentiation by squaring) for efficiency.
 * It panics if the exponent is negative, as this is disallowed for integer
 * exponentiation in Jophet and indicates a logic error that bypassed the
 * compiler's static checks.
 *
 * @param base The base of the operation.
 * @param exp The non-negative exponent.
 * @param file The name of the source file where the operation occurred.
 * @param line The line number in the source file where the operation occurred.
 * @return The result of `base` raised to the power of `exp`.
 * @note This function will cause program termination on failure (negative exponent).
 */
int64_t jophet_int_pow(int64_t base, int64_t exp, const char* file, int line);

/**
 * @brief Creates a vector containing a sequence of numbers.
 * 
 * Generates a `JophetVector` of integers or floats. It supports both a 2-argument
 * version (start, stop) with a default step of 1, and a 3-argument version with
 * a custom step. The range is inclusive.
 * 
 * @param elem_size The size of the elements (e.g., `sizeof(int64_t)`).
 * @param arg_count The number of numeric arguments (2 or 3).
 * @param ... The variable arguments: start, [step], stop.
 * @return A new JophetVector containing the generated sequence.
 */
JophetVector jophet_collect(size_t elem_size, int arg_count, ...);

/**
 * @brief Finds the minimum value in a generic collection, panicking if it's empty.
 * @param data A void pointer to the collection's data.
 * @param len The number of elements in the collection.
 * @param elem_size The size of each element.
 * @param compare_fn A function pointer to a type-specific comparison thunk.
 * @param file The source file where the call occurred.
 * @param line The line number where the call occurred.
 * @return A new heap-allocated pointer to a copy of the minimum element.
 * @note This is a generic implementation detail. The generated code will cast this
 *       to the correct type and dereference it. The caller does NOT own the memory.
 */
void* __jophet_collection_minimum_or_panic(const void* data, size_t len, size_t elem_size, Jophet_Comparison_Fn compare_fn, const char* file, int line);

/**
 * @brief Finds the maximum value in a generic collection, panicking if it's empty.
 * @param data A void pointer to the collection's data.
 * @param len The number of elements in the collection.
 * @param elem_size The size of each element.
 * @param compare_fn A function pointer to a type-specific comparison thunk.
 * @param file The source file where the call occurred.
 * @param line The line number where the call occurred.
 * @return A new heap-allocated pointer to a copy of the maximum element.
 * @note This is a generic implementation detail. The generated code will cast this
 *       to the correct type and dereference it. The caller does NOT own the memory.
 */
void* __jophet_collection_maximum_or_panic(const void* data, size_t len, size_t elem_size, Jophet_Comparison_Fn compare_fn, const char* file, int line);

/**
 * @brief Finds the minimum character in a string, panicking if it's empty.
 * @param data A pointer to the character data.
 * @param len The length of the string.
 * @param file The source file where the call occurred.
 * @param line The line number where the call occurred.
 * @return The minimum character value.
 */
char __jophet_string_minimum_or_panic(const char* data, size_t len, const char* file, int line);

/**
 * @brief Finds the maximum character in a string, panicking if it's empty.
 * @param data A pointer to the character data.
 * @param len The length of the string.
 * @param file The source file where the call occurred.
 * @param line The line number where the call occurred.
 * @return The maximum character value.
 */
char __jophet_string_maximum_or_panic(const char* data, size_t len, const char* file, int line);

#endif // JOPHET_RUNTIME_H