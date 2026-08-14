crate::define_param!(PyQuery, "Query");
crate::define_param!(PyPath, "Path");
crate::define_param!(PyCookie, "Cookie");
crate::define_param!(PyHeader, "Header", header);
crate::define_param!(PyBody, "Body", body);
crate::define_param!(PyForm, "Form", media: "application/x-www-form-urlencoded");
crate::define_param!(PyFile, "File", media: "multipart/form-data");

// Depends and Security aren't built using the macro because they have completely different fields
crate::define_param!(PyDepends, "Depends");
crate::define_param!(PySecurity, "Security");
