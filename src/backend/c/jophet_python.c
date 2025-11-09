// src/backend/c/jophet_python.c
#define PY_SSIZE_T_CLEAN
#include "jophet_python.h"
#include <stdarg.h>
#include <limits.h>

// Forward declaration for the recursive conversion helper
static PyObject* jophet_to_py_object(const void* data_ptr, JophetTypeTag type);

// Helper to check for and fetch a Python exception as a JophetString.
JophetString get_python_exception_string() {
    PyObject *ptype, *pvalue, *ptraceback;
    PyErr_Fetch(&ptype, &pvalue, &ptraceback);
    if (pvalue == NULL) {
        return String_new_from("Unknown Python exception");
    }

    PyObject* pstr = PyObject_Str(pvalue);
    if (pstr == NULL) {
        Py_XDECREF(ptype);
        Py_XDECREF(pvalue);
        Py_XDECREF(ptraceback);
        return String_new_from("Could not get string representation of Python exception");
    }

    const char* err_str = PyUnicode_AsUTF8(pstr);
    JophetString j_err = String_new_from(err_str);

    Py_XDECREF(pstr);
    Py_XDECREF(ptype);
    Py_XDECREF(pvalue);
    Py_XDECREF(ptraceback);

    return j_err;
}

// --- MACROS FOR VECTOR TO PYLIST CONVERSION ---

#define JOPHET_VECTOR_TO_PYLIST_SIGNED(FUNC_NAME, C_TYPE) \
static PyObject* FUNC_NAME(const JophetVector* vec) { \
    PyObject* pList = PyList_New(vec->len); \
    if (!pList) return NULL; \
    for (size_t i = 0; i < vec->len; i++) { \
        C_TYPE val = ((C_TYPE*)vec->data)[i]; \
        PyObject* pLong = PyLong_FromLongLong(val); \
        if (!pLong) { \
            Py_DECREF(pList); \
            return NULL; \
        } \
        PyList_SetItem(pList, i, pLong); \
    } \
    return pList; \
}

#define JOPHET_VECTOR_TO_PYLIST_UNSIGNED(FUNC_NAME, C_TYPE) \
static PyObject* FUNC_NAME(const JophetVector* vec) { \
    PyObject* pList = PyList_New(vec->len); \
    if (!pList) return NULL; \
    for (size_t i = 0; i < vec->len; i++) { \
        C_TYPE val = ((C_TYPE*)vec->data)[i]; \
        PyObject* pLong = PyLong_FromUnsignedLongLong(val); \
        if (!pLong) { \
            Py_DECREF(pList); \
            return NULL; \
        } \
        PyList_SetItem(pList, i, pLong); \
    } \
    return pList; \
}

#define JOPHET_VECTOR_TO_PYLIST_FLOAT(FUNC_NAME, C_TYPE) \
static PyObject* FUNC_NAME(const JophetVector* vec) { \
    PyObject* pList = PyList_New(vec->len); \
    if (!pList) return NULL; \
    for (size_t i = 0; i < vec->len; i++) { \
        C_TYPE val = ((C_TYPE*)vec->data)[i]; \
        PyObject* pFloat = PyFloat_FromDouble(val); \
        if (!pFloat) { \
            Py_DECREF(pList); \
            return NULL; \
        } \
        PyList_SetItem(pList, i, pFloat); \
    } \
    return pList; \
}

// --- GENERATED VECTOR TO PYLIST HELPERS ---

JOPHET_VECTOR_TO_PYLIST_SIGNED(jophet_vector_i8_to_pylist, int8_t)
JOPHET_VECTOR_TO_PYLIST_SIGNED(jophet_vector_i16_to_pylist, int16_t)
JOPHET_VECTOR_TO_PYLIST_SIGNED(jophet_vector_i32_to_pylist, int32_t)
JOPHET_VECTOR_TO_PYLIST_SIGNED(jophet_vector_i64_to_pylist, int64_t)

JOPHET_VECTOR_TO_PYLIST_UNSIGNED(jophet_vector_u8_to_pylist, uint8_t)
JOPHET_VECTOR_TO_PYLIST_UNSIGNED(jophet_vector_u16_to_pylist, uint16_t)
JOPHET_VECTOR_TO_PYLIST_UNSIGNED(jophet_vector_u32_to_pylist, uint32_t)
JOPHET_VECTOR_TO_PYLIST_UNSIGNED(jophet_vector_u64_to_pylist, uint64_t)

JOPHET_VECTOR_TO_PYLIST_FLOAT(jophet_vector_f32_to_pylist, float)
JOPHET_VECTOR_TO_PYLIST_FLOAT(jophet_vector_f64_to_pylist, double)

// --- SPECIALIZED VECTOR TO PYLIST HELPERS ---

// Helper to convert a JophetVector of JophetStrings to a PyList
static PyObject* jophet_vector_string_to_pylist(const JophetVector* vec) {
    PyObject* pList = PyList_New(vec->len);
    if (!pList) return NULL;
    for (size_t i = 0; i < vec->len; i++) {
        const JophetString* str = &(((const JophetString*)vec->data)[i]);
        PyObject* pStr = PyUnicode_FromStringAndSize(str->data, str->len);
        if (!pStr) {
            Py_DECREF(pList);
            return NULL;
        }
        PyList_SetItem(pList, i, pStr); // Steals reference to pStr
    }
    return pList;
}

// Helper to convert a JophetVector of bools to a PyList
static PyObject* jophet_vector_bool_to_pylist(const JophetVector* vec) {
    PyObject* pList = PyList_New(vec->len);
    if (!pList) return NULL;
    for (size_t i = 0; i < vec->len; i++) {
        bool val = ((bool*)vec->data)[i];
        PyObject* pBool = PyBool_FromLong(val);
        if (!pBool) {
            Py_DECREF(pList);
            return NULL;
        }
        PyList_SetItem(pList, i, pBool); // Steals reference to pBool
    }
    return pList;
}

// Helper to convert a JophetVector of chars to a PyList of 1-char strings
static PyObject* jophet_vector_char_to_pylist(const JophetVector* vec) {
    PyObject* pList = PyList_New(vec->len);
    if (!pList) return NULL;
    for (size_t i = 0; i < vec->len; i++) {
        char val[2] = { ((char*)vec->data)[i], '\0' };
        PyObject* pStr = PyUnicode_FromString(val);
        if (!pStr) {
            Py_DECREF(pList);
            return NULL;
        }
        PyList_SetItem(pList, i, pStr); // Steals reference to pStr
    }
    return pList;
}


// Helper to convert a JophetVector of JophetVectors of int64_t to a PyList of PyLists.
static PyObject* jophet_vector_vector_i64_to_pylist(const JophetVector* vec) {
    // The outer vector contains other JophetVector objects
    PyObject* pOuterList = PyList_New(vec->len);
    if (!pOuterList) return NULL;

    for (size_t i = 0; i < vec->len; i++) {
        // Get the inner vector
        const JophetVector* inner_vec = &(((const JophetVector*)vec->data)[i]);
        
        // Convert the inner vector of int64_t to a PyList
        PyObject* pInnerList = jophet_vector_i64_to_pylist(inner_vec);
        
        if (!pInnerList) {
            Py_DECREF(pOuterList);
            return NULL;
        }
        PyList_SetItem(pOuterList, i, pInnerList); // Steals reference to pInnerList
    }
    return pOuterList;
}

void jophet_py_init() {
    if (!Py_IsInitialized()) {
        Py_Initialize();
    }
}

void jophet_py_finalize() {
    if (Py_IsInitialized()) {
        Py_Finalize();
    }
}

void jophet_py_decref(PyObject* handle) {
    Py_XDECREF(handle);
}

Result_PythonModule_FfiError jophet_py_import(const char* module_name) {
    PyObject* pName = PyUnicode_DecodeFSDefault(module_name);
    if (!pName) {
        JophetString msg = String_new_from("Python FFI Error: Could not decode module name.");
        return (Result_PythonModule_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_PythonException, .data.Message = msg } };
    }

    PyObject* pModule = PyImport_Import(pName);
    Py_DECREF(pName);

    if (!pModule) {
        JophetString msg = get_python_exception_string();
        return (Result_PythonModule_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ModuleNotFound, .data.Message = msg } };
    }
    
    return (Result_PythonModule_FfiError){ .is_ok = true, .data.ok = pModule };
}

void jophet_py_print_object(PyObject* handle) {
    if (handle == NULL) {
        printf("<null Python handle>");
        return;
    }
    PyObject* pStr = PyObject_Str(handle);
    if (pStr == NULL) {
        // If getting the string representation of fails, clear the Python error and print a placeholder.
        PyErr_Clear();
        printf("<unprintable Python object>");
        return;
    }
    const char* cStr = PyUnicode_AsUTF8(pStr);
    if (cStr != NULL) {
        printf("%s", cStr);
    } else {
        PyErr_Clear();
        printf("<Python object with non-UTF8 representation>");
    }
    Py_DECREF(pStr);
}

// A generic helper to convert any supported Jophet type to a PyObject.
// `data_ptr` is always expected to be a `const void*` pointing to the data.
// For aggregate types, the generated C code is responsible for also generating
// a specific helper function (e.g., `__jophet_struct_to_py_dict_MyStruct`), and this
// function must be able to call it. This is handled by the Rust backend which generates
// a `jophet_to_py_object_dispatcher` function containing a switch statement.
// This function here is the fallback for primitives.
static PyObject* jophet_to_py_object(const void* data_ptr, JophetTypeTag type) {
    PyObject* pValue = NULL;
    switch(type) {
        case JOPHET_TYPE_ENUM:
        case JOPHET_TYPE_INT8:
        case JOPHET_TYPE_INT16:
        case JOPHET_TYPE_INT32:
        case JOPHET_TYPE_INT64:
            pValue = PyLong_FromLongLong(*(const int64_t*)data_ptr);
            break;
        case JOPHET_TYPE_UINT8:
        case JOPHET_TYPE_UINT16:
        case JOPHET_TYPE_UINT32:
        case JOPHET_TYPE_UINT64:
            pValue = PyLong_FromUnsignedLongLong(*(const uint64_t*)data_ptr);
            break;
        case JOPHET_TYPE_FLOAT32:
        case JOPHET_TYPE_FLOAT64:
            pValue = PyFloat_FromDouble(*(const double*)data_ptr);
            break;
        case JOPHET_TYPE_BOOL:
            pValue = PyBool_FromLong(*(const bool*)data_ptr);
            break;
        case JOPHET_TYPE_CHAR: {
            char c_val[2] = { *(const char*)data_ptr, '\0' };
            pValue = PyUnicode_FromString(c_val);
            break;
        }
        case JOPHET_TYPE_STRING:
            pValue = PyUnicode_FromStringAndSize(((const JophetString*)data_ptr)->data, ((const JophetString*)data_ptr)->len);
            break;
        case JOPHET_TYPE_STRING_SLICE:
            pValue = PyUnicode_FromString(*(const char**)data_ptr);
            break;
        case JOPHET_TYPE_VECTOR_I8: pValue = jophet_vector_i8_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_I16: pValue = jophet_vector_i16_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_I32: pValue = jophet_vector_i32_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_I64: pValue = jophet_vector_i64_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_U8: pValue = jophet_vector_u8_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_U16: pValue = jophet_vector_u16_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_U32: pValue = jophet_vector_u32_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_U64: pValue = jophet_vector_u64_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_F32: pValue = jophet_vector_f32_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_F64: pValue = jophet_vector_f64_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_STRING: pValue = jophet_vector_string_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_BOOL: pValue = jophet_vector_bool_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_CHAR: pValue = jophet_vector_char_to_pylist((const JophetVector*)data_ptr); break;
        case JOPHET_TYPE_VECTOR_VECTOR_I64:
            pValue = jophet_vector_vector_i64_to_pylist((const JophetVector*)data_ptr);
            break;
        case JOPHET_TYPE_TUPLE:
        case JOPHET_TYPE_STRUCT:
        case JOPHET_TYPE_DICTIONARY:
        case JOPHET_TYPE_TAGGED_UNION:
        case JOPHET_TYPE_ERROR: {
            // The `data_ptr` is now the actual data. The Rust backend generates
            // a specific helper for each aggregate type, and this function will call it.
            // Since C has no reflection, the Rust backend generates a new dispatcher function
            // with a switch statement that contains the calls to these helpers. We call that dispatcher.
            extern PyObject* jophet_to_py_object_dispatcher(const void* data, JophetTypeTag type);
            pValue = jophet_to_py_object_dispatcher(data_ptr, type);
            break;
        }
        case JOPHET_TYPE_PYTHON_SLICE:
            // The `data_ptr` is a pointer to the handle (PyObject**). Dereference it.
            pValue = *(PyObject**)data_ptr;
            Py_INCREF(pValue); // The caller will steal a reference, so we must increment.
            break;
        case JOPHET_TYPE_PYTHON_OBJECT:
            // This case handles a value that is already a Python object.
            // `data_ptr` points to the `PyObject*` handle. We dereference it,
            // increment the ref count, and return it.
            if (data_ptr != NULL) {
                pValue = *(PyObject**)data_ptr;
                if (pValue != NULL) {
                    Py_INCREF(pValue);
                }
            }
            break;
        default:
            fprintf(stderr, "Python FFI Error: Unsupported argument type for Python call.\n");
            break;
    }
    return pValue;
}

PythonObject jophet_py_call_method(PyObject* module, const char* method_name, int arg_count, ...) {
    if (!module) {
        fprintf(stderr, "Python FFI Error: Module or object handle is NULL.\n");
        return NULL;
    }

    PyObject* pFunc = PyObject_GetAttrString(module, method_name);
    if (!pFunc || !PyCallable_Check(pFunc)) {
        if (PyErr_Occurred()) PyErr_Print();
        fprintf(stderr, "Python FFI Error: Cannot find callable function '%s'.\n", method_name);
        Py_XDECREF(pFunc);
        return NULL;
    }

    PyObject* pArgs = PyTuple_New(arg_count);
    va_list vl;
    va_start(vl, arg_count);

    for (int i = 0; i < arg_count; i++) {
        void* data_ptr = va_arg(vl, void*);
        JophetTypeTag type = va_arg(vl, JophetTypeTag);

        PyObject* pValue = jophet_to_py_object(data_ptr, type);

        if (!pValue) {
            Py_DECREF(pArgs);
            Py_DECREF(pFunc);
            va_end(vl);
            return NULL;
        }
        PyTuple_SetItem(pArgs, i, pValue); // Steals reference
    }

    // After processing Jophet args, get the file and line info for panicking.
    const char* file = va_arg(vl, const char*);
    int line = va_arg(vl, int);
    va_end(vl);
    
    PyObject* pResult = PyObject_CallObject(pFunc, pArgs);
    Py_DECREF(pArgs);

    if (pResult == NULL) {
        JophetString err_msg = get_python_exception_string();
        Py_DECREF(pFunc);
        jophet_panic_on_py_err(&err_msg, file, line);
        return NULL; // Unreachable, but satisfies compiler
    }

    Py_DECREF(pFunc);
    return pResult; // The caller now owns this reference
}

PythonObject jophet_py_get_item(PythonObject object, const void* key_ptr, JophetTypeTag key_type, const char* file, int line) {
    if (!object) {
        fprintf(stderr, "Python FFI Error: Object handle is NULL for get_item call.\n");
        return NULL;
    }

    PyObject* pKey = jophet_to_py_object(key_ptr, key_type);
    if (!pKey) {
        // jophet_to_py_object does not set a Python exception, so we provide a generic message.
        JophetString err_msg = String_new_from("Failed to convert Jophet key to Python object for indexing.");
        jophet_panic_on_py_err(&err_msg, file, line);
        return NULL;
    }

    PyObject* pResult = PyObject_GetItem(object, pKey);
    Py_DECREF(pKey);

    if (pResult == NULL) {
        JophetString err_msg = get_python_exception_string();
        jophet_panic_on_py_err(&err_msg, file, line);
        return NULL; // Unreachable
    }

    return pResult; // Caller owns this reference
}

PythonObject jophet_py_get_attr(PythonObject object, const char* attr_name, const char* file, int line) {
    if (!object) {
        fprintf(stderr, "Python FFI Error: Object handle is NULL for get_attr call.\n");
        return NULL;
    }

    PyObject* pResult = PyObject_GetAttrString(object, attr_name);

    if (pResult == NULL) {
        JophetString err_msg = get_python_exception_string();
        jophet_panic_on_py_err(&err_msg, file, line);
        return NULL; // Unreachable
    }

    return pResult; // Caller owns this reference
}

uint64_t jophet_py_len_or_panic(PythonObject object, const char* file, int line) {
    if (!object) {
        fprintf(stderr, "Python FFI Error: Object handle is NULL for length call.\n");
        exit(1);
    }

    Py_ssize_t len = PyObject_Length(object);

    if (len < 0) {
        JophetString err_msg = get_python_exception_string();
        jophet_panic_on_py_err(&err_msg, file, line);
        return 0; // Unreachable
    }

    return (uint64_t)len;
}


PythonObject jophet_py_call_builtin_or_panic(const char* func_name, PythonObject arg, const char* file, int line) {
    PyObject* pBuiltins = PyEval_GetBuiltins();
    PyObject* pFunc = PyDict_GetItemString(pBuiltins, func_name);
    
    if (!pFunc || !PyCallable_Check(pFunc)) {
        char err_buf[128];
        snprintf(err_buf, sizeof(err_buf), "Internal FFI Error: Could not find Python built-in function '%s'.", func_name);
        JophetString err_msg = String_new_from(err_buf);
        jophet_panic_on_py_err(&err_msg, file, line);
        return NULL;
    }
    
    PyObject* pArgs = PyTuple_Pack(1, arg);
    PyObject* pResult = PyObject_CallObject(pFunc, pArgs);
    Py_DECREF(pArgs);

    if (pResult == NULL) {
        JophetString err_msg = get_python_exception_string();
        jophet_panic_on_py_err(&err_msg, file, line);
        return NULL; // Unreachable
    }
    
    return pResult;
}

PythonObject jophet_py_flatten_or_panic(PythonObject object, const char* file, int line) {
    PyObject* flat_list = PyList_New(0);
    if (!flat_list) {
        JophetString err_msg = String_new_from("Failed to create new list for flattening.");
        jophet_panic_on_py_err(&err_msg, file, line);
        return NULL;
    }

    PyObject* outer_iter = PyObject_GetIter(object);
    if (outer_iter == NULL) {
        Py_DECREF(flat_list);
        JophetString err_msg = get_python_exception_string();
        jophet_panic_on_py_err(&err_msg, file, line);
        return NULL;
    }

    PyObject* inner_list;
    while ((inner_list = PyIter_Next(outer_iter))) {
        PyObject* inner_iter = PyObject_GetIter(inner_list);
        if (inner_iter == NULL) {
            Py_DECREF(inner_list);
            Py_DECREF(outer_iter);
            Py_DECREF(flat_list);
            JophetString err_msg = get_python_exception_string();
            jophet_panic_on_py_err(&err_msg, file, line);
            return NULL;
        }

        PyObject* item;
        while ((item = PyIter_Next(inner_iter))) {
            if (PyList_Append(flat_list, item) != 0) {
                Py_DECREF(item);
                Py_DECREF(inner_iter);
                Py_DECREF(inner_list);
                Py_DECREF(outer_iter);
                Py_DECREF(flat_list);
                JophetString err_msg = get_python_exception_string();
                jophet_panic_on_py_err(&err_msg, file, line);
                return NULL;
            }
            Py_DECREF(item);
        }
        Py_DECREF(inner_iter);
        Py_DECREF(inner_list);

        if (PyErr_Occurred()) {
            Py_DECREF(outer_iter);
            Py_DECREF(flat_list);
            JophetString err_msg = get_python_exception_string();
            jophet_panic_on_py_err(&err_msg, file, line);
            return NULL;
        }
    }
    Py_DECREF(outer_iter);
    
    if (PyErr_Occurred()) {
        Py_DECREF(flat_list);
        JophetString err_msg = get_python_exception_string();
        jophet_panic_on_py_err(&err_msg, file, line);
        return NULL;
    }

    return flat_list;
}


// --- Conversion Functions ---

#define JOPHET_PY_CONVERT_SIGNED_INT(FUNC_NAME, RESULT_TYPE, C_TYPE, C_TYPE_MIN, C_TYPE_MAX) \
RESULT_TYPE FUNC_NAME(PythonObject handle) { \
    if (!PyLong_Check(handle)) { \
        JophetString msg = String_new_from("Object is not a Python int."); \
        return (RESULT_TYPE){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } }; \
    } \
    long long val = PyLong_AsLongLong(handle); \
    if (PyErr_Occurred() || val < C_TYPE_MIN || val > C_TYPE_MAX) { \
        JophetString msg = get_python_exception_string(); \
        if (msg.len == 0) { String_delete(&msg); msg = String_new_from("Value out of range for target integer type."); } \
        return (RESULT_TYPE){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } }; \
    } \
    return (RESULT_TYPE){ .is_ok = true, .data.ok = (C_TYPE)val }; \
}

#define JOPHET_PY_CONVERT_UNSIGNED_INT(FUNC_NAME, RESULT_TYPE, C_TYPE, C_TYPE_MAX) \
RESULT_TYPE FUNC_NAME(PythonObject handle) { \
    if (!PyLong_Check(handle)) { \
        JophetString msg = String_new_from("Object is not a Python int."); \
        return (RESULT_TYPE){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } }; \
    } \
    unsigned long long val = PyLong_AsUnsignedLongLong(handle); \
    if (PyErr_Occurred()) { \
        JophetString msg = get_python_exception_string(); \
        if (msg.len == 0) { String_delete(&msg); msg = String_new_from("Cannot convert negative value to unsigned type, or value is too large."); } \
        return (RESULT_TYPE){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } }; \
    } \
    if (val > C_TYPE_MAX) { \
        JophetString msg = String_new_from("Value out of range for target unsigned integer type."); \
        return (RESULT_TYPE){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } }; \
    } \
    return (RESULT_TYPE){ .is_ok = true, .data.ok = (C_TYPE)val }; \
}

JOPHET_PY_CONVERT_SIGNED_INT(jophet_py_convert_to_i8,  Result_int8_t_FfiError,  int8_t,  INT8_MIN,  INT8_MAX)
JOPHET_PY_CONVERT_SIGNED_INT(jophet_py_convert_to_i16, Result_int16_t_FfiError, int16_t, INT16_MIN, INT16_MAX)
JOPHET_PY_CONVERT_SIGNED_INT(jophet_py_convert_to_i32, Result_int32_t_FfiError, int32_t, INT32_MIN, INT32_MAX)
JOPHET_PY_CONVERT_SIGNED_INT(jophet_py_convert_to_i64, Result_int64_t_FfiError, int64_t, LLONG_MIN, LLONG_MAX)

JOPHET_PY_CONVERT_UNSIGNED_INT(jophet_py_convert_to_u8,  Result_uint8_t_FfiError,  uint8_t,  UCHAR_MAX)
JOPHET_PY_CONVERT_UNSIGNED_INT(jophet_py_convert_to_u16, Result_uint16_t_FfiError, uint16_t, USHRT_MAX)
JOPHET_PY_CONVERT_UNSIGNED_INT(jophet_py_convert_to_u32, Result_uint32_t_FfiError, uint32_t, UINT_MAX)
JOPHET_PY_CONVERT_UNSIGNED_INT(jophet_py_convert_to_u64, Result_uint64_t_FfiError, uint64_t, ULLONG_MAX)

Result_float_FfiError jophet_py_convert_to_f32(PythonObject handle) {
    if (!PyFloat_Check(handle) && !PyLong_Check(handle)) {
        JophetString msg = String_new_from("Object is not a Python float or int.");
        return (Result_float_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    double val = PyFloat_AsDouble(handle);
    if (PyErr_Occurred()) {
        JophetString msg = get_python_exception_string();
        return (Result_float_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    return (Result_float_FfiError){ .is_ok = true, .data.ok = (float)val };
}

Result_double_FfiError jophet_py_convert_to_f64(PythonObject handle) {
    if (!PyFloat_Check(handle) && !PyLong_Check(handle)) {
        JophetString msg = String_new_from("Object is not a Python float or int.");
        return (Result_double_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    double val = PyFloat_AsDouble(handle);
    if (PyErr_Occurred()) {
        JophetString msg = get_python_exception_string();
        return (Result_double_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    return (Result_double_FfiError){ .is_ok = true, .data.ok = val };
}

Result_bool_FfiError jophet_py_convert_to_bool(PythonObject handle) {
    if (!PyBool_Check(handle)) {
        JophetString msg = String_new_from("Object is not a Python bool.");
        return (Result_bool_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    return (Result_bool_FfiError){ .is_ok = true, .data.ok = (handle == Py_True) };
}

Result_char_FfiError jophet_py_convert_to_char(PythonObject handle) {
    if (!PyUnicode_Check(handle) || PyUnicode_GetLength(handle) != 1) {
        JophetString msg = String_new_from("Object is not a Python string of length 1.");
        return (Result_char_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    const char* str_val = PyUnicode_AsUTF8(handle);
    if (PyErr_Occurred()) {
       JophetString msg = get_python_exception_string();
       return (Result_char_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    return (Result_char_FfiError){ .is_ok = true, .data.ok = str_val[0] };
}

Result_JophetString_FfiError jophet_py_convert_to_string(PythonObject handle) {
    if (!PyUnicode_Check(handle)) {
        JophetString msg = String_new_from("Object is not a Python str.");
        return (Result_JophetString_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    const char* str_data = PyUnicode_AsUTF8(handle);
    if (!str_data) {
        JophetString msg = get_python_exception_string();
        return (Result_JophetString_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    return (Result_JophetString_FfiError){ .is_ok = true, .data.ok = String_new_from(str_data) };
}

Result_JophetVector_FfiError jophet_py_convert_to_vector_i64(PythonObject handle) {
    if (!PyList_Check(handle)) {
        JophetString msg = String_new_from("Object is not a Python list.");
        return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    JophetVector vec = Vector_new(sizeof(int64_t));
    Py_ssize_t size = PyList_Size(handle);
    for (Py_ssize_t i = 0; i < size; i++) {
        PyObject* item = PyList_GetItem(handle, i);
        if (!PyLong_Check(item)) {
            Vector_delete(&vec);
            JophetString msg = String_new_from("List element is not a Python int.");
            return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
        }
        int64_t val = PyLong_AsLongLong(item);
        if (PyErr_Occurred()) {
            Vector_delete(&vec);
            JophetString msg = get_python_exception_string();
            return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
        }
        Vector_push(&vec, &val);
    }
    return (Result_JophetVector_FfiError){ .is_ok = true, .data.ok = vec };
}

Result_JophetVector_FfiError jophet_py_convert_to_vector_f64(PythonObject handle) {
    if (!PyList_Check(handle)) {
        JophetString msg = String_new_from("Object is not a Python list.");
        return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    JophetVector vec = Vector_new(sizeof(double));
    Py_ssize_t size = PyList_Size(handle);
    for (Py_ssize_t i = 0; i < size; i++) {
        PyObject* item = PyList_GetItem(handle, i);
        if (!PyFloat_Check(item) && !PyLong_Check(item)) {
            Vector_delete(&vec);
            JophetString msg = String_new_from("List element is not a Python float or int.");
            return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
        }
        double val = PyFloat_AsDouble(item);
        if (PyErr_Occurred()) {
            Vector_delete(&vec);
            JophetString msg = get_python_exception_string();
            return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
        }
        Vector_push(&vec, &val);
    }
    return (Result_JophetVector_FfiError){ .is_ok = true, .data.ok = vec };
}

Result_JophetVector_FfiError jophet_py_convert_to_vector_string(PythonObject handle) {
    if (!PyList_Check(handle)) {
        JophetString msg = String_new_from("Object is not a Python list.");
        return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    JophetVector vec = Vector_new(sizeof(JophetString));
    Py_ssize_t size = PyList_Size(handle);
    for (Py_ssize_t i = 0; i < size; i++) {
        PyObject* item = PyList_GetItem(handle, i);
        if (!PyUnicode_Check(item)) {
            Vector_delete(&vec);
            JophetString msg = String_new_from("List element is not a Python str.");
            return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
        }
        const char* str_data = PyUnicode_AsUTF8(item);
        if (!str_data) {
            Vector_delete(&vec);
            JophetString msg = get_python_exception_string();
            return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
        }
        JophetString j_str = String_new_from(str_data);
        Vector_push(&vec, &j_str);
    }
    return (Result_JophetVector_FfiError){ .is_ok = true, .data.ok = vec };
}

Result_JophetVector_FfiError jophet_py_convert_to_vector_bool(PythonObject handle) {
    if (!PyList_Check(handle)) {
        JophetString msg = String_new_from("Object is not a Python list.");
        return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    JophetVector vec = Vector_new(sizeof(bool));
    Py_ssize_t size = PyList_Size(handle);
    for (Py_ssize_t i = 0; i < size; i++) {
        PyObject* item = PyList_GetItem(handle, i);
        if (!PyBool_Check(item)) {
            Vector_delete(&vec);
            JophetString msg = String_new_from("List element is not a Python bool.");
            return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
        }
        bool val = (item == Py_True);
        Vector_push(&vec, &val);
    }
    return (Result_JophetVector_FfiError){ .is_ok = true, .data.ok = vec };
}

Result_JophetVector_FfiError jophet_py_convert_to_vector_char(PythonObject handle) {
    if (!PyList_Check(handle)) {
        JophetString msg = String_new_from("Object is not a Python list.");
        return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
    }
    JophetVector vec = Vector_new(sizeof(char));
    Py_ssize_t size = PyList_Size(handle);
    for (Py_ssize_t i = 0; i < size; i++) {
        PyObject* item = PyList_GetItem(handle, i);
        if (!PyUnicode_Check(item) || PyUnicode_GetLength(item) != 1) {
            Vector_delete(&vec);
            JophetString msg = String_new_from("List element is not a Python string of length 1.");
            return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
        }
        const char* str_data = PyUnicode_AsUTF8(item);
        if (!str_data) {
            Vector_delete(&vec);
            JophetString msg = get_python_exception_string();
            return (Result_JophetVector_FfiError){ .is_ok = false, .data.err = { .tag = FfiError_ConversionFailed, .data.Message = msg } };
        }
        char val = str_data[0];
        Vector_push(&vec, &val);
    }
    return (Result_JophetVector_FfiError){ .is_ok = true, .data.ok = vec };
}