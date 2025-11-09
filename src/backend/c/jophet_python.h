// src/backend/c/jophet_python.h
#ifndef JOPHET_PYTHON_H
#define JOPHET_PYTHON_H

#include "runtime.h"
#include <Python.h> // This must be included

// Opaque type for a Python module handle in Jophet
typedef PyObject* PythonModule;
// Opaque type for a generic Python object handle in Jophet
typedef PyObject* PythonObject;


// Enum for passing type information from Jophet to the C runtime
typedef enum {
    JOPHET_TYPE_UNKNOWN,
    JOPHET_TYPE_INT8,
    JOPHET_TYPE_INT16,
    JOPHET_TYPE_INT32,
    JOPHET_TYPE_INT64,
    JOPHET_TYPE_UINT8,
    JOPHET_TYPE_UINT16,
    JOPHET_TYPE_UINT32,
    JOPHET_TYPE_UINT64,
    JOPHET_TYPE_FLOAT32,
    JOPHET_TYPE_FLOAT64,
    JOPHET_TYPE_BOOL,
    JOPHET_TYPE_CHAR,
    JOPHET_TYPE_STRING,
    JOPHET_TYPE_STRING_SLICE,
    JOPHET_TYPE_VECTOR_I8,
    JOPHET_TYPE_VECTOR_I16,
    JOPHET_TYPE_VECTOR_I32,
    JOPHET_TYPE_VECTOR_I64,
    JOPHET_TYPE_VECTOR_U8,
    JOPHET_TYPE_VECTOR_U16,
    JOPHET_TYPE_VECTOR_U32,
    JOPHET_TYPE_VECTOR_U64,
    JOPHET_TYPE_VECTOR_F32,
    JOPHET_TYPE_VECTOR_F64,
    JOPHET_TYPE_VECTOR_STRING,
    JOPHET_TYPE_VECTOR_BOOL,
    JOPHET_TYPE_VECTOR_CHAR,
    JOPHET_TYPE_VECTOR_VECTOR_I64,
    JOPHET_TYPE_ARRAY_I64,
    JOPHET_TYPE_ARRAY_F64,
    JOPHET_TYPE_TUPLE,
    JOPHET_TYPE_STRUCT,
    JOPHET_TYPE_DICTIONARY,
    JOPHET_TYPE_PYTHON_SLICE,  // For passing Python slice objects
    JOPHET_TYPE_ENUM,
    JOPHET_TYPE_TAGGED_UNION,
    JOPHET_TYPE_ERROR,
    // A specific tag for values that are already Python objects.
    JOPHET_TYPE_PYTHON_OBJECT,
} JophetTypeTag;

// The Result type for `importPy`.
typedef struct { bool is_ok; union { PythonModule ok; FfiError err; } data; } Result_PythonModule_FfiError;
// The Result type for fallible calls to Python built-ins.
typedef struct { bool is_ok; union { PythonObject ok; FfiError err; } data; } Result_PythonObject_FfiError;

/**
 * @brief Fetches the current Python exception details and returns them as a JophetString.
 * This is an internal helper for the FFI runtime.
 * @return A new JophetString containing the formatted exception message.
 */
JophetString get_python_exception_string();

/**
 * @brief Initializes the Python interpreter. Must be called once per process.
 *        This function is called automatically at the start of the `main` function
 *        if the Jophet program uses the Python FFI.
 */
void jophet_py_init(void);

/**
 * @brief Finalizes the Python interpreter. Should be called at program exit.
 *        This function is called automatically at the end of the `main` function
 *        if the Jophet program uses the Python FFI.
 */
void jophet_py_finalize(void);

/**
 * @brief De-references a Python object handle, decrementing its reference count.
 * This is the cleanup function for `PythonModule` and `PythonObject` types.
 * @param handle The handle to the Python object. Can be NULL.
 */
void jophet_py_decref(PyObject* handle);

/**
 * @brief Imports a Python module. Assumes the interpreter has already been initialized.
 * @param module_name The name of the module to import (e.g., "matplotlib.pyplot").
 * @return A `Result` struct. On success, `is_ok` is true and `data.ok` contains a handle
 *         to the module object. On failure, `is_ok` is false and `data.err` contains
 *         a structured `FfiError`.
 */
Result_PythonModule_FfiError jophet_py_import(const char* module_name);

/**
 * @brief Prints the string representation of a Python object to stdout.
 * This is equivalent to calling `str()` on the object in Python.
 * @param handle The handle to the Python object.
 */
void jophet_py_print_object(PyObject* handle);

/**
 * @brief Calls a method on a Python module or object with a variable number of arguments.
 *        If the Python call fails, this function will panic and terminate the program.
 * @param module A handle to the Python module or object.
 * @param method_name The name of the function to call.
 * @param arg_count The number of Jophet arguments being passed.
 * @param ... A sequence of (void* data_ptr, JophetTypeTag type) pairs, where `data_ptr`
 *            is a pointer to the argument's data. This is followed by (const char* file, int line)
 *            for error reporting.
 * @return A new, opaque `PythonObject` handle to the result of the Python call.
 * @note This function will cause program termination on failure.
 *       The caller is responsible for the reference count of the returned object.
 */
PythonObject jophet_py_call_method(PyObject* module, const char* method_name, int arg_count, ...);

/**
 * @brief Calls a built-in Python function (like `min` or `max`) on a single PythonObject argument.
 *        This version can fail and returns a Result.
 * @param func_name The name of the built-in function to call (e.g., "min").
 * @param arg The PythonObject to pass as an argument.
 * @return A `Result` struct containing the `PythonObject` on success, or an `FfiError` on failure.
 */
Result_PythonObject_FfiError jophet_py_call_builtin_fallible(const char* func_name, PythonObject arg);

/**
 * @brief Calls a built-in Python function (like `min` or `max`) on a single PythonObject argument.
 *        This version will panic and terminate the program if the Python call fails.
 * @param func_name The name of the built-in function to call (e.g., "min").
 * @param arg The PythonObject to pass as an argument.
 * @param file The name of the source file where the call occurred.
 * @param line The line number in the source file where the call occurred.
 * @return A new, opaque `PythonObject` handle to the result of the Python call.
 * @note This function will cause program termination on failure.
 *       The caller is responsible for the reference count of the returned object.
 */
PythonObject jophet_py_call_builtin_or_panic(const char* func_name, PythonObject arg, const char* file, int line);

/**
 * @brief Gets an item from a Python object using the `[]` operator (PyObject_GetItem).
 *        If the Python call fails, this function will panic and terminate the program.
 * @param object A handle to the Python object to index.
 * @param key_ptr A void pointer to the key data (e.g., a JophetString, an integer).
 * @param key_type An enum tag identifying the type of the key.
 * @param file The name of the source file where the access occurred.
 * @param line The line number in the source file where the access occurred.
 * @return A new, opaque `PythonObject` handle to the resulting item.
 * @note This function will cause program termination on failure.
 *       The caller is responsible for the reference count of the returned object.
 */
PythonObject jophet_py_get_item(PythonObject object, const void* key_ptr, JophetTypeTag key_type, const char* file, int line);

/**
 * @brief Gets an attribute from a Python object using the `.` operator (PyObject_GetAttrString).
 *        If the Python call fails, this function will panic and terminate the program.
 * @param object A handle to the Python object from which to get the attribute.
 * @param attr_name A null-terminated C string representing the attribute name.
 * @param file The name of the source file where the access occurred.
 * @param line The line number in the source file where the access occurred.
 * @return A new, opaque `PythonObject` handle to the resulting attribute.
 * @note This function will cause program termination on failure.
 *       The caller is responsible for the reference count of the returned object.
 */
PythonObject jophet_py_get_attr(PythonObject object, const char* attr_name, const char* file, int line);

/**
 * @brief Gets the length of a Python object, equivalent to `len(obj)`.
 *        If the operation fails, this function will panic and terminate the program.
 * @param object A handle to a Python object.
 * @param file The name of the source file where the call occurred.
 * @param line The line number in the source file where the call occurred.
 * @return The length of the object as a 64-bit unsigned integer.
 * @note This function will cause program termination on failure.
 */
uint64_t jophet_py_len_or_panic(PythonObject object, const char* file, int line);

/**
 * @brief Flattens a nested Python iterable into a new Python list.
 *        If the object is not iterable or an error occurs during iteration, this function
 *        will panic and terminate the program.
 * @param object A handle to a Python object, expected to be a nested iterable (e.g., a list of lists).
 * @param file The name of the source file where the call occurred.
 * @param line The line number in the source file where the call occurred.
 * @return A new, opaque `PythonObject` handle to the flattened Python list.
 * @note This function will cause program termination on failure.
 *       The caller is responsible for the reference count of the returned object.
 */
PythonObject jophet_py_flatten_or_panic(PythonObject object, const char* file, int line);


// --- Conversion Functions (PythonObject -> Jophet Type) ---

// Result types for each conversion function.
typedef struct { bool is_ok; union { int8_t ok; FfiError err; } data; } Result_int8_t_FfiError;
typedef struct { bool is_ok; union { int16_t ok; FfiError err; } data; } Result_int16_t_FfiError;
typedef struct { bool is_ok; union { int32_t ok; FfiError err; } data; } Result_int32_t_FfiError;
typedef struct { bool is_ok; union { int64_t ok; FfiError err; } data; } Result_int64_t_FfiError;
typedef struct { bool is_ok; union { uint8_t ok; FfiError err; } data; } Result_uint8_t_FfiError;
typedef struct { bool is_ok; union { uint16_t ok; FfiError err; } data; } Result_uint16_t_FfiError;
typedef struct { bool is_ok; union { uint32_t ok; FfiError err; } data; } Result_uint32_t_FfiError;
typedef struct { bool is_ok; union { uint64_t ok; FfiError err; } data; } Result_uint64_t_FfiError;
typedef struct { bool is_ok; union { float ok; FfiError err; } data; } Result_float_FfiError;
typedef struct { bool is_ok; union { double ok; FfiError err; } data; } Result_double_FfiError;
typedef struct { bool is_ok; union { bool ok; FfiError err; } data; } Result_bool_FfiError;
typedef struct { bool is_ok; union { char ok; FfiError err; } data; } Result_char_FfiError;
typedef struct { bool is_ok; union { JophetString ok; FfiError err; } data; } Result_JophetString_FfiError;
typedef struct { bool is_ok; union { JophetVector ok; FfiError err; } data; } Result_JophetVector_FfiError;
typedef struct { bool is_ok; union { JophetDictionary ok; FfiError err; } data; } Result_JophetDictionary_FfiError;

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet Int8.
 * Fails if the Python object is not a Python `int` or if its value is out of range for an 8-bit signed integer.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `int8_t` on success, or an `FfiError` on failure.
 */
Result_int8_t_FfiError jophet_py_convert_to_i8(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet Int16.
 * Fails if the Python object is not a Python `int` or if its value is out of range for a 16-bit signed integer.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `int16_t` on success, or an `FfiError` on failure.
 */
Result_int16_t_FfiError jophet_py_convert_to_i16(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet Int32.
 * Fails if the Python object is not a Python `int` or if its value is out of range for a 32-bit signed integer.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `int32_t` on success, or an `FfiError` on failure.
 */
Result_int32_t_FfiError jophet_py_convert_to_i32(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet Int64.
 * Fails if the Python object is not a Python `int` or if its value is out of range for a 64-bit signed integer.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `int64_t` on success, or an `FfiError` on failure.
 */
Result_int64_t_FfiError jophet_py_convert_to_i64(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet UInt8.
 * Fails if the Python object is not a non-negative Python `int` or if its value is out of range for an 8-bit unsigned integer.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `uint8_t` on success, or an `FfiError` on failure.
 */
Result_uint8_t_FfiError jophet_py_convert_to_u8(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet UInt16.
 * Fails if the Python object is not a non-negative Python `int` or if its value is out of range for a 16-bit unsigned integer.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `uint16_t` on success, or an `FfiError` on failure.
 */
Result_uint16_t_FfiError jophet_py_convert_to_u16(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet UInt32.
 * Fails if the Python object is not a non-negative Python `int` or if its value is out of range for a 32-bit unsigned integer.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `uint32_t` on success, or an `FfiError` on failure.
 */
Result_uint32_t_FfiError jophet_py_convert_to_u32(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet UInt64.
 * Fails if the Python object is not a non-negative Python `int` or if its value is out of range for a 64-bit unsigned integer.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `uint64_t` on success, or an `FfiError` on failure.
 */
Result_uint64_t_FfiError jophet_py_convert_to_u64(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet Float32 (float).
 * Fails if the Python object is not a Python `float` or `int`.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `float` on success, or an `FfiError` on failure.
 */
Result_float_FfiError jophet_py_convert_to_f32(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet Float64 (double).
 * Fails if the Python object is not a Python `float` or `int`.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `double` on success, or an `FfiError` on failure.
 */
Result_double_FfiError jophet_py_convert_to_f64(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet Bool.
 * Fails if the Python object is not a Python `bool`.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `bool` on success, or an `FfiError` on failure.
 */
Result_bool_FfiError jophet_py_convert_to_bool(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet Char.
 * Fails if the Python object is not a Python `str` of length 1.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the `char` on success, or an `FfiError` on failure.
 */
Result_char_FfiError jophet_py_convert_to_char(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle to a Jophet String.
 * Fails if the Python object is not a Python `str`. The string data is copied.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the new `JophetString` on success, or an `FfiError` on failure.
 */
Result_JophetString_FfiError jophet_py_convert_to_string(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle (representing a list of ints) to a Jophet Vector<Int64>.
 * Fails if the Python object is not a `list` or if any element in the list is not a Python `int`.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the new `JophetVector` on success, or an `FfiError` on failure.
 */
Result_JophetVector_FfiError jophet_py_convert_to_vector_i64(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle (representing a list of floats) to a Jophet Vector<Float64>.
 * Fails if the Python object is not a `list` or if any element in the list is not a Python `float` or `int`.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the new `JophetVector` on success, or an `FfiError` on failure.
 */
Result_JophetVector_FfiError jophet_py_convert_to_vector_f64(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle (representing a list of strings) to a Jophet Vector<String>.
 * Fails if the Python object is not a `list` or if any element in the list is not a Python `str`.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the new `JophetVector` on success, or an `FfiError` on failure.
 */
Result_JophetVector_FfiError jophet_py_convert_to_vector_string(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle (representing a list of booleans) to a Jophet Vector<Bool>.
 * Fails if the Python object is not a `list` or if any element in the list is not a Python `bool`.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the new `JophetVector` on success, or an `FfiError` on failure.
 */
Result_JophetVector_FfiError jophet_py_convert_to_vector_bool(PythonObject handle);

/**
 * @brief Attempts to convert a PythonObject handle (representing a list of 1-char strings) to a Jophet Vector<Char>.
 * Fails if the Python object is not a `list` or if any element in the list is not a Python `str` of length 1.
 * @param handle The opaque handle to the Python object.
 * @return A Result struct containing the new `JophetVector` on success, or an `FfiError` on failure.
 */
Result_JophetVector_FfiError jophet_py_convert_to_vector_char(PythonObject handle);

#endif // JOPHET_PYTHON_H